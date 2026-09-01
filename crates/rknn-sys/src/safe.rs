//! Безопасная обёртка над RKNN C API. Copy-режим (rknn_inputs_set /
//! rknn_outputs_get) — см. ADR-006: zero-copy rknn_create_mem на этой
//! сборке librknnrt 2.3.0 (apt, Radxa) уходит в DRM/GBM и падает SIGSEGV.

use std::ffi::c_void;
use std::fs;
use std::ptr::null_mut;

use thiserror::Error;

use crate::ffi::*;

#[derive(Debug, Error)]
pub enum RknnError {
    #[error("не удалось открыть файл модели: {0}")]
    ModelFile(String),
    #[error("rknn_init failed: {0}")]
    Init(i32),
    #[error("rknn_query {what} failed: {0}")]
    Query { what: &'static str, code: i32 },
    #[error("rknn_set_core_mask failed: {0}")]
    CoreMask(i32),
    #[error("rknn_set_input_shapes failed: {0}")]
    SetInputShapes(i32),
    #[error("rknn_inputs_set failed: {0}")]
    InputsSet(i32),
    #[error("rknn_run failed: {0}")]
    Run(i32),
    #[error("rknn_outputs_get failed: {0}")]
    OutputsGet(i32),
    #[error("кадр меньше тензора: {frame} байт < {need}")]
    FrameTooSmall { frame: usize, need: usize },
    #[error("модель не загружена")]
    NotLoaded,
}

pub type RknnResult<T> = std::result::Result<T, RknnError>;

/// Загруженная RKNN-модель (copy-режим IO).
///
/// НЕ потокобезопасна: один экземпляр — один поток (детектор-воркер).
pub struct RknnModel {
    ctx: rknn_context,
    /// Вход (w, h) тензора — для letterbox у вызывающего.
    pub input_w: u32,
    pub input_h: u32,
    pub n_output: usize,
    output_attrs: Vec<rknn_tensor_attr>,
}

impl RknnModel {
    /// Загрузить модель из файла.
    ///
    /// `shape`: желаемый вход (w, h) для динамических моделей
    /// (rknn_set_input_shapes). Для статических моделей применяется
    /// безусловно и должен совпадать со встроенным.
    pub fn load(path: &str, shape: Option<(u32, u32)>) -> RknnResult<Self> {
        let data = fs::read(path)
            .map_err(|e| RknnError::ModelFile(format!("{path}: {e}")))?;

        let mut ctx: rknn_context = 0;
        let ret = unsafe {
            rknn_init(
                &mut ctx,
                data.as_ptr() as *mut c_void,
                data.len() as u32,
                0,
                null_mut(),
            )
        };
        if ret < 0 {
            return Err(RknnError::Init(ret));
        }

        // Ядро NPU 0 — на драйвере 2.3.0 AUTO-планировка наблюдалась
        // segfault-ами (перенос из rknn-bridge Autotargeting).
        let ret = unsafe { rknn_set_core_mask(ctx, RKNN_NPU_CORE_0) };
        if ret < 0 {
            tracing::warn!(code = ret, "rknn_set_core_mask failed, продолжаем");
        }

        let mut io = rknn_input_output_num::default();
        query(ctx, RKNN_QUERY_IN_OUT_NUM, &mut io, "IN_OUT_NUM")?;
        tracing::info!(
            inputs = io.n_input,
            outputs = io.n_output,
            "RKNN модель инициализирована"
        );

        let mut model = Self {
            ctx,
            input_w: 0,
            input_h: 0,
            n_output: io.n_output as usize,
            output_attrs: Vec::new(),
        };

        if let Some((w, h)) = shape {
            model.apply_input_shape(w, h)?;
        }
        model.query_io()?;
        Ok(model)
    }

    /// Установить входную форму (динамические модели).
    fn apply_input_shape(&mut self, w: u32, h: u32) -> RknnResult<()> {
        let mut attr = rknn_tensor_attr::default();
        attr.index = 0;
        query(self.ctx, RKNN_QUERY_INPUT_ATTR, &mut attr, "INPUT_ATTR")?;
        tracing::info!(
            n_dims = attr.n_dims,
            dims = ?attr.dims.iter().take(attr.n_dims as usize).collect::<Vec<_>>(),
            "вход модели до установки формы"
        );
        if attr.n_dims == 4 {
            // Layout по fmt: NHWC → [N,H,W,C] (H,W — dims[1],dims[2]),
            // NCHW → [N,C,H,W] (H,W — dims[2],dims[3]). Реальная модель bkb
            // отдаёт NHWC с формами [1,1088,1088,3] / [1,640,640,3].
            if attr.fmt == RKNN_TENSOR_NHWC {
                attr.dims[1] = h;
                attr.dims[2] = w;
            } else {
                attr.dims[2] = h;
                attr.dims[3] = w;
            }
            let ret = unsafe { rknn_set_input_shapes(self.ctx, 1, &mut attr) };
            if ret < 0 {
                return Err(RknnError::SetInputShapes(ret));
            }
        }
        Ok(())
    }

    /// Запросить формы входа/выходов (для letterbox и декодера).
    fn query_io(&mut self) -> RknnResult<()> {
        let mut attr = rknn_tensor_attr::default();
        attr.index = 0;
        query(self.ctx, RKNN_QUERY_INPUT_ATTR, &mut attr, "INPUT_ATTR")?;
        self.input_w = tensor_width(&attr);
        self.input_h = tensor_height(&attr);
        tracing::info!(w = self.input_w, h = self.input_h, "входной тензор");

        for i in 0..self.n_output as u32 {
            let mut oa = rknn_tensor_attr::default();
            oa.index = i;
            query(self.ctx, RKNN_QUERY_OUTPUT_ATTR, &mut oa, "OUTPUT_ATTR")?;
            tracing::debug!(
                index = i,
                dims = ?oa.dims.iter().take(oa.n_dims as usize).collect::<Vec<_>>(),
                n_elems = oa.n_elems,
                "выход модели"
            );
            self.output_attrs.push(oa);
        }
        Ok(())
    }

    /// Инференс на RGB24-кадре (letterboxed до input_w × input_h, NHWC).
    /// Возвращает Vec<f32> на каждый выход (copy-режим, want_float=1).
    pub fn infer(&mut self, rgb: &[u8]) -> RknnResult<Vec<Vec<f32>>> {
        let tw = self.input_w as usize;
        let th = self.input_h as usize;
        let needed = tw * th * 3;
        if rgb.len() < needed {
            return Err(RknnError::FrameTooSmall { frame: rgb.len(), need: needed });
        }

        // 1) inputs_set: UINT8 / NHWC, runtime сам нормализует и квантует.
        let mut input = rknn_input::new(
            0,
            rgb.as_ptr() as *mut c_void,
            needed as u32,
            RKNN_TENSOR_UINT8,
            RKNN_TENSOR_NHWC,
        );
        let ret = unsafe { rknn_inputs_set(self.ctx, 1, &mut input) };
        if ret < 0 {
            return Err(RknnError::InputsSet(ret));
        }

        // 2) run.
        let ret = unsafe { rknn_run(self.ctx, null_mut()) };
        if ret < 0 {
            return Err(RknnError::Run(ret));
        }

        // 3) outputs_get: просим float32 (runtime конвертирует fp16/int8).
        let n = self.n_output as u32;
        let mut outputs: Vec<rknn_output> = (0..n).map(rknn_output::want_float).collect();
        let ret = unsafe { rknn_outputs_get(self.ctx, n, outputs.as_mut_ptr(), null_mut()) };
        if ret < 0 {
            return Err(RknnError::OutputsGet(ret));
        }

        let mut result = Vec::with_capacity(outputs.len());
        for out in &outputs {
            let n_floats = out.size as usize / 4;
            let slice = if out.buf.is_null() || n_floats == 0 {
                &[][..]
            } else {
                unsafe { std::slice::from_raw_parts(out.buf as *const f32, n_floats) }
            };
            result.push(slice.to_vec());
        }

        // 4) release (освобождает буферы, выделенные рантаймом).
        unsafe { rknn_outputs_release(self.ctx, n, outputs.as_mut_ptr()) };
        Ok(result)
    }

    /// Формы выходов (dims каждого выхода).
    pub fn output_dims(&self) -> Vec<Vec<u32>> {
        self.output_attrs
            .iter()
            .map(|a| a.dims.iter().take(a.n_dims as usize).copied().collect())
            .collect()
    }
}

impl Drop for RknnModel {
    fn drop(&mut self) {
        unsafe {
            if self.ctx != 0 {
                rknn_destroy(self.ctx);
            }
        }
    }
}

fn query<T>(ctx: rknn_context, cmd: i32, out: &mut T, what: &'static str) -> RknnResult<()> {
    let ret = unsafe {
        rknn_query(
            ctx,
            cmd,
            out as *mut T as *mut c_void,
            std::mem::size_of::<T>() as u32,
        )
    };
    if ret < 0 {
        Err(RknnError::Query { what, code: ret })
    } else {
        Ok(())
    }
}

/// Ширина входного тензора: NHWC → dims[2], NCHW → dims[3].
fn tensor_width(attr: &rknn_tensor_attr) -> u32 {
    let d: Vec<u32> = attr.dims.iter().take(attr.n_dims as usize).copied().collect();
    if d.len() == 4 {
        if attr.fmt == RKNN_TENSOR_NHWC {
            d[2]
        } else {
            d[3]
        }
    } else {
        0
    }
}

/// Высота входного тензора: NHWC → dims[1], NCHW → dims[2].
fn tensor_height(attr: &rknn_tensor_attr) -> u32 {
    let d: Vec<u32> = attr.dims.iter().take(attr.n_dims as usize).copied().collect();
    if d.len() == 4 {
        if attr.fmt == RKNN_TENSOR_NHWC {
            d[1]
        } else {
            d[2]
        }
    } else {
        0
    }
}

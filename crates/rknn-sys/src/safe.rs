//! Безопасная обёртка над RKNN C API с zero-copy IO.
//! Rust-порт RknnBackend из rknn-bridge (Autotargeting), см. ADR-002.

use std::ffi::c_void;
use std::fs;
use std::ptr::{null, null_mut};

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
    #[error("rknn_create_mem failed ({what})")]
    CreateMem { what: &'static str },
    #[error("rknn_set_io_mem failed ({what}): {0}")]
    SetIoMem { what: &'static str, code: i32 },
    #[error("rknn_run failed: {0}")]
    Run(i32),
    #[error("кадр меньше тензора: {frame} байт < {need}")]
    FrameTooSmall { frame: usize, need: usize },
    #[error("модель не загружена")]
    NotLoaded,
    #[error("rknn_set_input_shapes failed: {0}")]
    SetInputShapes(i32),
}

pub type RknnResult<T> = std::result::Result<T, RknnError>;

/// Загруженная RKNN-модель с выделенным zero-copy буферами.
///
/// НЕ потокобезопасна: создавайте по одному экземпляру на поток
/// (детектор-воркер владеет ею единолично).
pub struct RknnModel {
    ctx: rknn_context,
    input_attr: rknn_tensor_attr,
    output_attrs: Vec<rknn_tensor_attr>,
    input_mem: *mut rknn_tensor_mem,
    output_mems: Vec<*mut rknn_tensor_mem>,
    /// (w, h) входного тензора в текущем виде.
    pub input_w: u32,
    pub input_h: u32,
    /// Число выходов модели.
    pub n_output: usize,
    /// Число классов не храним — его выводит декодер по форме выходов.
}

// Внутри сырые указатели на память, выделенную рантаймом; владение
// эксклюзивное (кстати, поэтому автоматический Send/Sync здесь и не нужен).

impl RknnModel {
    /// Загрузить модель из файла.
    ///
    /// `shape`: желаемая (w, h) входа. Для динамических моделей применяется
    /// через rknn_set_input_shapes; для статических должна совпадать с
    /// встроенной (иначе ошибка рантайма при первом run).
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

        // Привязка к ядру NPU 0 — см. комментарий в шапке крейта (segfault на 2.3.0).
        let ret = unsafe { rknn_set_core_mask(ctx, RKNN_NPU_CORE_0) };
        if ret < 0 {
            tracing::warn!(code = ret, "rknn_set_core_mask failed, продолжаем на AUTO");
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
            input_attr: Default::default(),
            output_attrs: Vec::new(),
            input_mem: null_mut(),
            output_mems: Vec::new(),
            input_w: 0,
            input_h: 0,
            n_output: io.n_output as usize,
        };

        // Динамическая форма: применяем желаемую до выделения памяти.
        if let Some((w, h)) = shape {
            model.apply_input_shape(w, h)?;
        }

        model.setup_io()?;
        Ok(model)
    }

    /// Попытаться задать входную форму (только для динамических моделей).
    /// Статическим моделям не вредит: rknn_set_input_shapes вернёт ошибку,
    /// которую мы логируем и игнорируем, если форма уже такая.
    fn apply_input_shape(&mut self, w: u32, h: u32) -> RknnResult<()> {
        let mut attr = rknn_tensor_attr::default();
        attr.index = 0;
        let ret =
            unsafe { rknn_query(self.ctx, RKNN_QUERY_INPUT_ATTR, &mut attr as *mut _ as *mut c_void, std::mem::size_of::<rknn_tensor_attr>() as u32) };
        if ret < 0 {
            return Err(RknnError::Query { what: "INPUT_ATTR", code: ret });
        }
        tracing::info!(
            n_dims = attr.n_dims,
            dims = ?attr.dims.iter().take(attr.n_dims as usize).collect::<Vec<_>>(),
            "вход модели до установки формы"
        );
        if attr.n_dims == 4 {
            // Предполагаем NCHW [N, C, H, W] — H и W последние.
            attr.dims[2] = h;
            attr.dims[3] = w;
            let ret = unsafe { rknn_set_input_shapes(self.ctx, 1, &attr) };
            if ret < 0 {
                return Err(RknnError::SetInputShapes(ret));
            }
        }
        Ok(())
    }

    /// Форсировать типы IO и выделить zero-copy буферы (один раз при загрузке).
    fn setup_io(&mut self) -> RknnResult<()> {
        // === ВХОД: UINT8 / NHWC ===
        let mut attr = rknn_tensor_attr::default();
        attr.index = 0;
        query(self.ctx, RKNN_QUERY_INPUT_ATTR, &mut attr, "INPUT_ATTR")?;
        attr.type_ = RKNN_TENSOR_UINT8;
        attr.fmt = RKNN_TENSOR_NHWC;
        attr.h_stride = 0;
        self.input_attr = attr;
        self.input_w = tensor_width(&attr);
        self.input_h = tensor_height(&attr);
        tracing::info!(
            w = self.input_w,
            h = self.input_h,
            size_with_stride = self.input_attr.size_with_stride,
            "входной тензор (UINT8/NHWC)"
        );

        // === ВЫХОДЫ: FLOAT32 ===
        for i in 0..self.n_output as u32 {
            let mut oa = rknn_tensor_attr::default();
            oa.index = i;
            query(self.ctx, RKNN_QUERY_OUTPUT_ATTR, &mut oa, "OUTPUT_ATTR")?;
            oa.type_ = RKNN_TENSOR_FLOAT32;
            self.output_attrs.push(oa);
        }

        // === Выделение памяти ===
        let in_sz = self.input_attr.size_with_stride.max(self.input_attr.size);
        let mem = unsafe { rknn_create_mem(self.ctx, in_sz) };
        if mem.is_null() {
            return Err(RknnError::CreateMem { what: "input" });
        }
        let ret = unsafe { rknn_set_io_mem(self.ctx, mem, &self.input_attr) };
        if ret < 0 {
            return Err(RknnError::SetIoMem { what: "input", code: ret });
        }
        self.input_mem = mem;

        for oa in &self.output_attrs {
            let sz = oa.size_with_stride.max(oa.n_elems.saturating_mul(4));
            let mem = unsafe { rknn_create_mem(self.ctx, sz) };
            if mem.is_null() {
                return Err(RknnError::CreateMem { what: "output" });
            }
            let ret = unsafe { rknn_set_io_mem(self.ctx, mem, oa as *const _ as *mut _) };
            if ret < 0 {
                return Err(RknnError::SetIoMem { what: "output", code: ret });
            }
            self.output_mems.push(mem);
        }
        Ok(())
    }

    /// Прогнать инференс на RGB24-кадре (w*h*3 байт, уже letterboxed до
    /// размеров входного тензора). Возвращает по Vec<f32> на каждый выход.
    pub fn infer(&mut self, rgb: &[u8]) -> RknnResult<Vec<Vec<f32>>> {
        if self.input_mem.is_null() {
            return Err(RknnError::NotLoaded);
        }
        let tw = self.input_w as usize;
        let th = self.input_h as usize;
        let row_bytes = tw * 3;
        let w_stride = if self.input_attr.w_stride > 0 {
            self.input_attr.w_stride as usize
        } else {
            row_bytes
        };
        let needed = row_bytes * th;
        if rgb.len() < needed {
            return Err(RknnError::FrameTooSmall { frame: rgb.len(), need: needed });
        }
        unsafe {
            let dst = (*self.input_mem).virt_addr as *mut u8;
            if w_stride == row_bytes {
                std::ptr::copy_nonoverlapping(rgb.as_ptr(), dst, needed);
            } else {
                for y in 0..th {
                    std::ptr::copy_nonoverlapping(
                        rgb.as_ptr().add(y * row_bytes),
                        dst.add(y * w_stride),
                        row_bytes,
                    );
                }
            }
            let ret = rknn_run(self.ctx, null());
            if ret < 0 {
                return Err(RknnError::Run(ret));
            }
            let mut outs = Vec::with_capacity(self.output_mems.len());
            for (mem, attr) in self.output_mems.iter().zip(self.output_attrs.iter()) {
                let src = (**mem).virt_addr as *const f32;
                let n = attr.n_elems as usize;
                let slice = std::slice::from_raw_parts(src, n);
                outs.push(slice.to_vec());
            }
            Ok(outs)
        }
    }

    /// Формы выходов (dims каждого выхода, как их отдаёт рантайм).
    pub fn output_dims(&self) -> Vec<Vec<u32>> {
        self.output_attrs
            .iter()
            .map(|a| a.dims.iter().take(a.n_dims as usize).copied().collect())
            .collect()
    }
}

impl Drop for RknnModel {
    fn drop(&mut self) {
        // Порядок критичен: память освобождается при живом контексте.
        unsafe {
            for mem in self.output_mems.drain(..) {
                rknn_destroy_mem(self.ctx, mem);
            }
            if !self.input_mem.is_null() {
                rknn_destroy_mem(self.ctx, self.input_mem);
            }
            if self.ctx >= 0 {
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

/// Ширина входного тензора по dims: NHWC → dims[2], NCHW → dims[3].
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

/// Высота входного тензора по dims: NHWC → dims[1], NCHW → dims[2].
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

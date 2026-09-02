//! NPU-бэкенд трекера на RKNN (фаза C роадмапа, ADR-010).
//!
//! Модели из tools/convert_nanotrack.py: backbone 127/255 — int8 (mmse,
//! входы UINT8/NHWC), голова — fp16 (входы FLOAT32/NCHW — фичи).
//! Кроп приходит RGB u8 — для int8-моделей уходит как есть.

use crate::{NanoError, NanoResult, TrackerNets};
use crate::imgops::Img;
use rknn_sys::{RknnModel, TensorData, ffi::RKNN_NPU_CORE_1};

pub struct RknnNets {
    z: RknnModel,
    x: RknnModel,
    head: RknnModel,
    /// Индексы входов головы: input1 = zf (шаблон), input2 = xf (поиск).
    head_zf_input: u32,
    head_xf_input: u32,
    /// Индексы выходов головы: cls и bbox (по именам из конвертации).
    head_cls_output: usize,
    head_bbox_output: usize,
    swap_rb: bool,
}

impl RknnNets {
    pub fn load(
        z_path: &str,
        x_path: &str,
        head_path: &str,
        swap_rb: bool,
    ) -> NanoResult<Self> {
        // Ядро 1: детектор занимает ядро 0 — разводим конкурирующие задачи.
        let load = |p: &str| {
            RknnModel::load_on_core(p, None, RKNN_NPU_CORE_1).map_err(|e| {
                NanoError::ModelLoad {
                    path: p.to_string(),
                    source: anyhow::anyhow!("{e}"),
                }
            })
        };
        let z = load(z_path)?;
        let x = load(x_path)?;
        let head = load(head_path)?;

        let head_zf_input = head
            .input_index_by_name("input1")
            .unwrap_or(0);
        let head_xf_input = head
            .input_index_by_name("input2")
            .unwrap_or(1);
        // Выходы конвертации: ".../cls_pred/..." и "/Exp" (bbox);
        // запасной порядок — по размеру (cls 2·16·16=512 < bbox 4·16·16=1024).
        let (head_cls_output, head_bbox_output) =
            match (head.output_index_by_name("cls"), head.output_index_by_name("Exp")) {
                (Some(c), Some(b)) => (c, b),
                _ => (0, 1),
            };
        tracing::info!(
            zf = head_zf_input,
            xf = head_xf_input,
            cls = head_cls_output,
            bbox = head_bbox_output,
            "голова NanoTrack (RKNN): карта входов/выходов"
        );

        Ok(Self {
            z,
            x,
            head,
            head_zf_input,
            head_xf_input,
            head_cls_output,
            head_bbox_output,
            swap_rb,
        })
    }

    /// Диагностика: прогон головы с NHWC-плоскими float-входами.
    #[allow(dead_code)]
    pub fn head_probe(
        &mut self,
        zf_nhwc: &[f32],
        xf_nhwc: &[f32],
    ) -> NanoResult<(Vec<f32>, Vec<f32>)> {
        let outs = self
            .head
            .infer_inputs(&[
                (self.head_zf_input, TensorData::Float32Nhwc(zf_nhwc)),
                (self.head_xf_input, TensorData::Float32Nhwc(xf_nhwc)),
            ])
            .map_err(|e| NanoError::Inference(anyhow::anyhow!("head probe: {e}")))?;
        Ok((
            outs.get(self.head_cls_output).cloned().unwrap_or_default(),
            outs.get(self.head_bbox_output).cloned().unwrap_or_default(),
        ))
    }

    fn run_backbone(&mut self, which: bool, crop: &Img) -> NanoResult<Vec<f32>> {
        let data = if self.swap_rb {
            // Модели сконвертированы под RGB; при необходимости меняем каналы.
            let mut sw = crop.data.clone();
            for px in sw.chunks_exact_mut(3) {
                px.swap(0, 2);
            }
            sw
        } else {
            crop.data.clone()
        };
        let model = if which { &mut self.x } else { &mut self.z };
        let outs = model
            .infer(&data)
            .map_err(|e| NanoError::Inference(anyhow::anyhow!("backbone RKNN: {e}")))?;
        outs.into_iter()
            .next()
            .ok_or_else(|| NanoError::BadOutputShape("backbone без выходов".into()))
    }
}

/// NCHW-плоский [C,H,W] → NHWC-плоский [H,W,C].
fn nchw_to_nhwc(src: &[f32], c: usize, h: usize, w: usize) -> Vec<f32> {
    let mut dst = vec![0f32; src.len()];
    for y in 0..h {
        for x in 0..w {
            for ch in 0..c {
                dst[(y * w + x) * c + ch] = src[ch * h * w + y * w + x];
            }
        }
    }
    dst
}

impl TrackerNets for RknnNets {
    fn run_backbone_z(&mut self, crop: &Img) -> NanoResult<Vec<f32>> {
        self.run_backbone(false, crop)
    }

    fn run_backbone_x(&mut self, crop: &Img) -> NanoResult<Vec<f32>> {
        self.run_backbone(true, crop)
    }

    fn run_head(&mut self, zf: &[f32], xf: &[f32]) -> NanoResult<(Vec<f32>, Vec<f32>)> {
        // QUIRK librknnrt 2.3.0: float-входы мульти-входовых графов драйвер
        // ждёт в NHWC-плоском порядке даже при attr.fmt=NCHW (симулятор x86
        // при этом честен к NCHW). Диагностика 2026-09-02: NCHW cls cos 0.89,
        // NHWC — 0.993/0.9996. Переставляем [C,H,W] → [H,W,C].
        let zf_nhwc = nchw_to_nhwc(zf, 48, 8, 8);
        let xf_nhwc = nchw_to_nhwc(xf, 48, 16, 16);
        let outs = self
            .head
            .infer_inputs(&[
                (self.head_zf_input, TensorData::Float32Nhwc(&zf_nhwc)),
                (self.head_xf_input, TensorData::Float32Nhwc(&xf_nhwc)),
            ])
            .map_err(|e| NanoError::Inference(anyhow::anyhow!("head RKNN: {e}")))?;
        let cls = outs
            .get(self.head_cls_output)
            .cloned()
            .ok_or_else(|| NanoError::BadOutputShape("head без cls-выхода".into()))?;
        let bbox = outs
            .get(self.head_bbox_output)
            .cloned()
            .ok_or_else(|| NanoError::BadOutputShape("head без bbox-выхода".into()))?;
        Ok((cls, bbox))
    }
}

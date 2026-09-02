//! CPU-бэкенд трекера на tract (чистый Rust ONNX). Исходная реализация
//! NanoTracker до рефакторинга на трейт TrackerNets (ADR-010).

use crate::{NanoError, NanoResult, TrackerNets};
use crate::imgops::{to_nchw_f32, Img};
use tract_onnx::prelude::*;

/// Псевдоним загруженной tract-модели (стандартная тройка дженериков).
type Model = RunnableModel<TypedFact, Box<dyn TypedOp>, Graph<TypedFact, Box<dyn TypedOp>>>;

pub struct TractNets {
    backbone_z: Model,
    backbone_x: Model,
    head: Model,
    /// Позиционные индексы входов головы: (template, search).
    head_template_input: usize,
    /// Позиционные индексы выходов головы: (cls, bbox).
    head_cls_output: usize,
    head_bbox_output: usize,
    swap_rb: bool,
}

impl TractNets {
    pub fn load(
        backbone_z_path: &str,
        backbone_x_path: &str,
        head_path: &str,
        swap_rb: bool,
    ) -> NanoResult<Self> {
        let load = |p: &str| -> NanoResult<Model> {
            tract_onnx::onnx()
                .model_for_path(p)
                .map_err(|e| NanoError::ModelLoad {
                    path: p.to_string(),
                    source: e.into(),
                })?
                .into_typed()
                .map_err(|e| NanoError::ModelLoad {
                    path: p.to_string(),
                    source: e.into(),
                })?
                .into_optimized()
                .map_err(|e| NanoError::ModelLoad {
                    path: p.to_string(),
                    source: e.into(),
                })?
                .into_runnable()
                .map_err(|e| NanoError::ModelLoad {
                    path: p.to_string(),
                    source: e.into(),
                })
        };
        let backbone_z = load(backbone_z_path)?;
        let backbone_x = load(backbone_x_path)?;
        let head = load(head_path)?;

        // Смысловые индексы входов/выходов головы по именам узлов.
        let node_name = |m: &Model, outlet: &OutletId| -> String {
            m.model().nodes()[outlet.node].name.clone()
        };
        let head_in_names: Vec<String> =
            head.model().inputs.iter().map(|o| node_name(&head, o)).collect();
        let head_out_names: Vec<String> = head
            .model()
            .outputs
            .iter()
            .map(|o| node_name(&head, o))
            .collect();
        tracing::info!(
            inputs = ?head_in_names,
            outputs = ?head_out_names,
            "head модели (tract): входы/выходы"
        );

        let head_template_input = head_in_names
            .iter()
            .position(|n| n.contains("input1"))
            .unwrap_or(0);
        let head_cls_output = head_out_names
            .iter()
            .position(|n| n.contains("output1") || n.contains("cls"))
            .unwrap_or(0);
        let head_bbox_output = if head_cls_output == 0 { 1 } else { 0 };

        Ok(Self {
            backbone_z,
            backbone_x,
            head,
            head_template_input,
            head_cls_output,
            head_bbox_output,
            swap_rb,
        })
    }

    fn run_backbone(
        model: &mut Model,
        swap_rb: bool,
        crop: &Img,
        sz: u32,
    ) -> NanoResult<Vec<f32>> {
        let blob = to_nchw_f32(crop, swap_rb);
        let input = Tensor::from_shape(&[1usize, 3, sz as usize, sz as usize], &blob)
            .map_err(|e| NanoError::BadOutputShape(format!("входной тензор: {e}")))?;
        let feats = model.run(tvec!(TValue::from(input)))?;
        let arr = feats[0]
            .to_array_view::<f32>()
            .map_err(|e| NanoError::BadOutputShape(format!("выход backbone: {e}")))?;
        Ok(arr.iter().cloned().collect())
    }
}

impl TrackerNets for TractNets {
    fn run_backbone_z(&mut self, crop: &Img) -> NanoResult<Vec<f32>> {
        Self::run_backbone(&mut self.backbone_z, self.swap_rb, crop, 127)
    }

    fn run_backbone_x(&mut self, crop: &Img) -> NanoResult<Vec<f32>> {
        Self::run_backbone(&mut self.backbone_x, self.swap_rb, crop, 255)
    }

    fn run_head(&mut self, zf: &[f32], xf: &[f32]) -> NanoResult<(Vec<f32>, Vec<f32>)> {
        let zf_t = Tensor::from_shape(&[1usize, 48, 8, 8], zf)
            .map_err(|e| NanoError::BadOutputShape(format!("zf: {e}")))?;
        let xf_t = Tensor::from_shape(&[1usize, 48, 16, 16], xf)
            .map_err(|e| NanoError::BadOutputShape(format!("xf: {e}")))?;
        let outputs = if self.head_template_input == 0 {
            self.head
                .run(tvec!(TValue::from(zf_t), TValue::from(xf_t)))?
        } else {
            self.head
                .run(tvec!(TValue::from(xf_t), TValue::from(zf_t)))?
        };
        let read = |v: &TValue, what: &str| -> NanoResult<Vec<f32>> {
            let arr = v
                .to_array_view::<f32>()
                .map_err(|e| NanoError::BadOutputShape(format!("{what}: {e}")))?;
            Ok(arr.iter().cloned().collect())
        };
        let cls = read(&outputs[self.head_cls_output], "cls")?;
        let bbox = read(&outputs[self.head_bbox_output], "bbox")?;
        Ok((cls, bbox))
    }
}

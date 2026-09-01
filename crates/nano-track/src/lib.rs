//! NanoTrack на Rust через tract (чистый ONNX-рантайм).
//!
//! Дословный порт OpenCV TrackerNanoImpl (modules/video/src/tracking/
//! tracker_nano.cpp,OpenCV 4.x; сам он — адаптация NanoTrack от HonglinChu).
//! Числовой паритет важен: bkb гонял именно OpenCV-реализацию в поле.
//!
//! Сохранена даже особенность оригинала: в scale-penalty используется
//! sizeCal(targetPos) — позиция цели вместо размера (см. update()).
//!
//! Модели: nanotrack_backbone_sim.onnx + nanotrack_head_sim.onnx (из bkb,
//! origin — OpenCV Zoo). template 127×127, search 255×255, scoreSize 16.

pub mod imgops;
pub mod kalman;
pub mod stabilizer;

use common::BBox;
use tract_onnx::prelude::*;
pub use stabilizer::Stabilizer;

use imgops::{get_subwindow, to_nchw_f32, Img};

/// Псевдоним загруженной tract-модели (стандартная тройка дженериков).
type Model = RunnableModel<TypedFact, Box<dyn TypedOp>, Graph<TypedFact, Box<dyn TypedOp>>>;

/// Ошибки трекера.
#[derive(Debug, thiserror::Error)]
pub enum NanoError {
    #[error("не удалось загрузить модель {path}: {source}")]
    ModelLoad {
        path: String,
        source: anyhow::Error,
    },
    #[error("инференс tract: {0}")]
    Inference(#[from] anyhow::Error),
    #[error("неожиданная форма выхода трекера: {0}")]
    BadOutputShape(String),
}

pub type NanoResult<T> = std::result::Result<T, NanoError>;

const EXEMPLAR_SIZE: f32 = 127.0;
const INSTANCE_SIZE: f32 = 255.0;
const TOTAL_STRIDE: i32 = 16;

struct TrackerConfig {
    window_influence: f32,
    lr: f32,
    context_amount: f32,
    penalty_k: f32,
}

impl Default for TrackerConfig {
    fn default() -> Self {
        Self {
            window_influence: 0.455,
            lr: 0.37,
            context_amount: 0.5,
            penalty_k: 0.055,
        }
    }
}

pub struct NanoTracker {
    /// Два закреплённых экземпляра backbone: для шаблона 127×127 и поиска
    /// 255×255. Модель динамическая, и tract при смене размера на живом
    /// графе конфликтует символами («255 != 127»); фиксированные входы
    /// позволяют ещё и оптимизировать каждый граф под свой размер.
    backbone_z: Model,
    backbone_x: Model,
    head: Model,
    /// Позиционные индексы входов головы: (template, search).
    head_template_input: usize,
    head_search_input: usize,
    /// Позиционные индексы выходов головы: (cls, bbox).
    head_cls_output: usize,
    head_bbox_output: usize,
    cfg: TrackerConfig,
    score_size: usize,
    /// Порядок каналов входного блоба: true = BGR→RGB (мы кормим RGB, поэтому false).
    swap_rb: bool,

    // --- состояние цели ---
    target_pos: [f32; 2],
    target_sz: [f32; 2],
    img_size: (u32, u32),
    hanning: Vec<f32>,
    grid_x: Vec<f32>,
    grid_y: Vec<f32>,
    template: Option<Tensor>,
    tracking_score: f32,
}

impl NanoTracker {
    /// Загрузить модели. `backbone_z_path` — backbone для шаблона 127×127,
    /// `backbone_x_path` — для поиска 255×255 (модели статические, отдельные).
    pub fn new(
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

        // Определяем смысловые индексы входов/выходов головы по именам.
        let node_name = |m: &Model, outlet: &OutletId| -> String {
            m.model().nodes()[outlet.node].name.clone()
        };
        let head_in_names: Vec<String> =
            head.model().inputs.iter().map(|o| node_name(&head, o)).collect();
        let head_out_names: Vec<String> =
            head.model().outputs.iter().map(|o| node_name(&head, o)).collect();
        tracing::info!(
            inputs = ?head_in_names,
            outputs = ?head_out_names,
            "head модели: входы/выходы"
        );

        let head_template_input = head_in_names
            .iter()
            .position(|n| n.contains("input1"))
            .unwrap_or(0);
        let head_search_input = head_in_names
            .iter()
            .position(|n| n.contains("input2"))
            .unwrap_or_else(|| (head_in_names.len() - 1).min(1));
        let head_cls_output = head_out_names
            .iter()
            .position(|n| n.contains("output1"))
            .unwrap_or(0);
        let head_bbox_output = head_out_names
            .iter()
            .position(|n| n.contains("output2"))
            .unwrap_or_else(|| (head_out_names.len() - 1).min(1));

        let score_size = ((INSTANCE_SIZE as i32 - EXEMPLAR_SIZE as i32) / TOTAL_STRIDE + 8) as usize;

        Ok(Self {
            backbone_z,
            backbone_x,
            head,
            head_template_input,
            head_search_input,
            head_cls_output,
            head_bbox_output,
            cfg: TrackerConfig::default(),
            score_size,
            swap_rb,
            target_pos: [0.0; 2],
            target_sz: [0.0; 2],
            img_size: (0, 0),
            hanning: hanning_window(score_size),
            grid_x: Vec::new(),
            grid_y: Vec::new(),
            template: None,
            tracking_score: 0.0,
        })
    }

    /// Инициализация на новом боксе.
    pub fn init(&mut self, img: &Img, bbox: BBox) -> NanoResult<()> {
        self.cfg = TrackerConfig::default();
        self.img_size = (img.w, img.h);

        self.target_pos = [bbox.x + bbox.w * 0.5, bbox.y + bbox.h * 0.5];
        self.target_sz = [bbox.w, bbox.h];

        // Расширяем бокс контекстом.
        let sum_sz = self.target_sz[0] + self.target_sz[1];
        let w_extent = self.target_sz[0] + self.cfg.context_amount * sum_sz;
        let h_extent = self.target_sz[1] + self.cfg.context_amount * sum_sz;
        let sz = (w_extent * h_extent).sqrt() as i32;

        let crop = get_subwindow(img, self.target_pos[0], self.target_pos[1], sz, 127);
        let blob = to_nchw_f32(&crop, self.swap_rb);
        let input = tensor_from_blob(&blob, 127)?;
        let feats = self.backbone_z.run(tvec!(TValue::from(input)))?;
        // Шаблон — это выход backbone с init (вход input1 головы).
        self.template = Some(feats[0].clone().into_tensor());

        self.generate_grids();
        self.tracking_score = 0.0;
        Ok(())
    }

    /// Обновить по кадру. Возвращает бокс и score (через tracking_score()).
    pub fn update(&mut self, img: &Img) -> NanoResult<BBox> {
        let Some(template) = self.template.clone() else {
            // Без init — возвращаем вырожденный бокс (вызов стороны обязан звать init).
            return Ok(BBox::new(0.0, 0.0, 0.0, 0.0));
        };

        let target_sz_sum = self.target_sz[0] + self.target_sz[1];
        let wc = self.target_sz[0] + self.cfg.context_amount * target_sz_sum;
        let hc = self.target_sz[1] + self.cfg.context_amount * target_sz_sum;
        let sz = (wc * hc).sqrt();
        let scale_z = EXEMPLAR_SIZE / sz;
        let sx = sz * (INSTANCE_SIZE / EXEMPLAR_SIZE);
        self.target_sz[0] *= scale_z;
        self.target_sz[1] *= scale_z;

        let crop = get_subwindow(img, self.target_pos[0], self.target_pos[1], sx as i32, 255);
        let blob = to_nchw_f32(&crop, self.swap_rb);
        let search = tensor_from_blob(&blob, 255)?;
        let search_feat = self.backbone_x.run(tvec!(TValue::from(search)))?;

        // Раскладываем входы головы по вычисленным индексам (их ровно два).
        let outputs = if self.head_template_input == 0 {
            self.head.run(tvec!(
                TValue::from(template),
                TValue::from(search_feat[0].clone().into_tensor())
            ))?
        } else {
            self.head.run(tvec!(
                TValue::from(search_feat[0].clone().into_tensor()),
                TValue::from(template)
            ))?
        };

        let cls = flat_f32(&outputs[self.head_cls_output], "cls")?;
        let bbox_pred = flat_f32(&outputs[self.head_bbox_output], "bbox")?;

        let ss = self.score_size;
        if cls.len() != 2 * ss * ss || bbox_pred.len() != 4 * ss * ss {
            return Err(NanoError::BadOutputShape(format!(
                "cls len {} (ожидалось {}), bbox len {} (ожидалось {})",
                cls.len(),
                2 * ss * ss,
                bbox_pred.len(),
                4 * ss * ss
            )));
        }

        // softmax по 2 строкам cls, берём строку 1 как score.
        let mut score = vec![0f32; ss * ss];
        for i in 0..ss * ss {
            let r0 = cls[i];
            let r1 = cls[ss * ss + i];
            let m = r0.max(r1);
            let e0 = (r0 - m).exp();
            let e1 = (r1 - m).exp();
            score[i] = e1 / (e0 + e1);
        }

        // Предсказанные стороны бокса в координатах search-окна.
        let mut pred_x1 = vec![0f32; ss * ss];
        let mut pred_y1 = vec![0f32; ss * ss];
        let mut pred_x2 = vec![0f32; ss * ss];
        let mut pred_y2 = vec![0f32; ss * ss];
        for i in 0..ss * ss {
            let bx0 = bbox_pred[i];
            let by0 = bbox_pred[ss * ss + i];
            let bx1 = bbox_pred[2 * ss * ss + i];
            let by1 = bbox_pred[3 * ss * ss + i];
            pred_x1[i] = self.grid_x[i] - bx0;
            pred_y1[i] = self.grid_y[i] - by0;
            pred_x2[i] = self.grid_x[i] + bx1;
            pred_y2[i] = self.grid_y[i] + by1;
        }

        // === Пенальти (дословно из OpenCV-порта) ===
        // scale penalty; ВНИМАНИЕ: оригинал делит на sizeCal(targetPos) —
        // особенность OpenCV-порта, сохраняем для числового паритета с bkb.
        let sc_denom = size_cal(self.target_pos[0], self.target_pos[1]);
        let mut sc = vec![0f32; ss * ss];
        for i in 0..ss * ss {
            let v = size_cal(pred_x2[i] - pred_x1[i], pred_y2[i] - pred_y1[i]) / sc_denom;
            sc[i] = reciprocal_max(v);
        }

        // ratio penalty
        let ratio_val = self.target_sz[0] / self.target_sz[1].max(1e-6);
        let mut rc = vec![0f32; ss * ss];
        for i in 0..ss * ss {
            let w = (pred_x2[i] - pred_x1[i]).max(1e-6);
            let h = (pred_y2[i] - pred_y1[i]).max(1e-6);
            rc[i] = reciprocal_max(ratio_val / (w / h));
        }

        let mut penalty = vec![0f32; ss * ss];
        let mut pscore = vec![0f32; ss * ss];
        for i in 0..ss * ss {
            penalty[i] = ((rc[i] * sc[i] - 1.0) * self.cfg.penalty_k * -1.0).exp();
            pscore[i] = penalty[i] * score[i];
            pscore[i] = pscore[i] * (1.0 - self.cfg.window_influence)
                + self.hanning[i] * self.cfg.window_influence;
        }

        // Аргмакс pscore.
        let mut best = 0usize;
        let mut best_val = f32::NEG_INFINITY;
        for (i, &v) in pscore.iter().enumerate() {
            if v > best_val {
                best_val = v;
                best = i;
            }
        }
        self.tracking_score = best_val;

        let x1_val = pred_x1[best];
        let x2_val = pred_x2[best];
        let y1_val = pred_y1[best];
        let y2_val = pred_y2[best];

        let pred_xs = (x1_val + x2_val) * 0.5;
        let pred_ys = (y1_val + y2_val) * 0.5;
        let pred_w = (x2_val - x1_val) / scale_z;
        let pred_h = (y2_val - y1_val) / scale_z;

        let diff_xs = (pred_xs - INSTANCE_SIZE / 2.0) / scale_z;
        let diff_ys = (pred_ys - INSTANCE_SIZE / 2.0) / scale_z;

        self.target_sz[0] /= scale_z;
        self.target_sz[1] /= scale_z;

        let lr = penalty[best] * score[best] * self.cfg.lr;

        let mut res_x = self.target_pos[0] + diff_xs;
        let mut res_y = self.target_pos[1] + diff_ys;
        let mut res_w = pred_w * lr + (1.0 - lr) * self.target_sz[0];
        let mut res_h = pred_h * lr + (1.0 - lr) * self.target_sz[1];

        let (img_w, img_h) = (self.img_size.0 as f32, self.img_size.1 as f32);
        res_x = res_x.clamp(0.0, img_w);
        res_y = res_y.clamp(0.0, img_h);
        res_w = res_w.clamp(10.0, img_w);
        res_h = res_h.clamp(10.0, img_h);

        self.target_pos = [res_x, res_y];
        self.target_sz = [res_w, res_h];

        Ok(BBox::new(res_x - res_w / 2.0, res_y - res_h / 2.0, res_w, res_h))
    }

    /// Score последнего update (качество сопровождения).
    pub fn tracking_score(&self) -> f32 {
        self.tracking_score
    }

    /// Инициализирован ли трекер.
    pub fn is_initialized(&self) -> bool {
        self.template.is_some()
    }

    fn generate_grids(&mut self) {
        let sz = self.score_size as i32;
        let sz2 = sz / 2;
        let mut base = vec![0f32; sz as usize];
        for (i, v) in base.iter_mut().enumerate() {
            *v = (i as i32 - sz2) as f32;
        }
        self.grid_x = Vec::with_capacity(base.len() * base.len());
        self.grid_y = Vec::with_capacity(base.len() * base.len());
        for &y in &base {
            for &x in &base {
                self.grid_x.push(x * TOTAL_STRIDE as f32 + INSTANCE_SIZE / 2.0);
                self.grid_y.push(y * TOTAL_STRIDE as f32 + INSTANCE_SIZE / 2.0);
            }
        }
    }
}

/// Окно Ханнинга scoreSize×scoreSize (симметричное, как cv::createHanningWindow).
fn hanning_window(n: usize) -> Vec<f32> {
    let mut w = vec![0f32; n * n];
    let denom = if n > 1 { (n - 1) as f32 } else { 1.0 };
    for y in 0..n {
        let wy = 0.5 * (1.0 - (2.0 * std::f32::consts::PI * y as f32 / denom).cos());
        for x in 0..n {
            let wx = 0.5 * (1.0 - (2.0 * std::f32::consts::PI * x as f32 / denom).cos());
            w[y * n + x] = wy * wx;
        }
    }
    w
}

#[inline]
fn size_cal(w: f32, h: f32) -> f32 {
    let pad = (w + h) * 0.5;
    ((w + pad) * (h + pad)).sqrt()
}

#[inline]
fn reciprocal_max(v: f32) -> f32 {
    v.max(1.0 / v)
}

/// Плоское (row-major) чтение выхода tract в Vec<f32>.
fn flat_f32(v: &TValue, what: &str) -> NanoResult<Vec<f32>> {
    let arr = v
        .to_array_view::<f32>()
        .map_err(|e| NanoError::BadOutputShape(format!("{what}: {e}")))?;
    Ok(arr.iter().cloned().collect())
}

fn tensor_from_blob(blob: &[f32], sz: u32) -> NanoResult<Tensor> {
    Tensor::from_shape(&[1usize, 3, sz as usize, sz as usize], blob)
        .map_err(|e| NanoError::BadOutputShape(format!("входной тензор: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hanning_corner_is_zero() {
        let w = hanning_window(16);
        assert!(w[0].abs() < 1e-6);
        // Центр окна максимален.
        let center = w[7 * 16 + 7];
        assert!(w.iter().all(|&v| v <= center + 1e-6));
    }

    #[test]
    fn grids_shape() {
        let mut t = NanoTrackerShim::new();
        t.generate_grids();
        assert_eq!(t.grid_x.len(), 256);
        // Центр сетки = INSTANCE_SIZE/2 со сдвигом полклетки:
        // минимальная координата = (0-8)*16 + 127.5 = -0.5
        assert!((t.grid_x[0] + 0.5).abs() < 1e-4, "grid_x[0]={}", t.grid_x[0]);
    }

    /// Обёртка для тестов без моделей.
    struct NanoTrackerShim {
        grid_x: Vec<f32>,
        grid_y: Vec<f32>,
    }
    impl NanoTrackerShim {
        fn new() -> Self {
            Self {
                grid_x: Vec::new(),
                grid_y: Vec::new(),
            }
        }
        fn generate_grids(&mut self) {
            let sz = 16i32;
            let sz2 = sz / 2;
            let base: Vec<f32> = (0..sz).map(|i| (i - sz2) as f32).collect();
            for &y in &base {
                for &x in &base {
                    self.grid_x.push(x * 16.0 + 127.5);
                    self.grid_y.push(y * 16.0 + 127.5);
                }
            }
        }
    }
}

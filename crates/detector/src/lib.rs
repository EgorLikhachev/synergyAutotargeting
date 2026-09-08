//! YOLOv8-декодер для RKNN-выходов. Поддерживает два layout-а:
//!
//! 1. **YoloBranches (6 выходов)**: 3 ветки × (box [1,64,Gh,Gw], cls [1,Nc,Gh,Gw]).
//!    DFL-декод, координаты через grid+stride. Классы уже после sigmoid
//!    (экспорт по рецепту rknn_model_zoo). Порт декодера yolov8_utils предшественника.
//! 2. **SingleHead (1 выход)**: [1, 4+Nc, A] в пикселях 640-пространства,
//!    классы — сырые логиты, нужен sigmoid (ADR D-010).
//!
//! Layout выбирается автоматически по числу выходов и их форме.

use common::{BBox, Detection};

/// Параметры letterbox-препроцессинга и обратного преобразования.
#[derive(Debug, Clone, Copy)]
pub struct LetterboxParams {
    pub scale: f32,
    pub pad_x: f32,
    pub pad_y: f32,
    pub target: u32,
}

impl LetterboxParams {
    /// Точка из letterbox-пространства в исходное.
    pub fn unproject_xy(&self, x: f32, y: f32) -> (f32, f32) {
        let inv = 1.0 / self.scale;
        ((x - self.pad_x) * inv, (y - self.pad_y) * inv)
    }
}

/// Letterbox кадра RGB24 в квадрат target×target с заполнением 114.
/// Возвращает готовые NHWC-байты и параметры обратного преобразования.
pub fn letterbox_rgb24(
    rgb: &[u8],
    w: u32,
    h: u32,
    target: u32,
) -> (Vec<u8>, LetterboxParams) {
    let scale = (target as f32 / w as f32).min(target as f32 / h as f32);
    let new_w = (w as f32 * scale).round() as u32;
    let new_h = (h as f32 * scale).round() as u32;
    let pad_x = ((target - new_w) / 2) as i32;
    let pad_y = ((target - new_h) / 2) as i32;

    let mut out = vec![114u8; (target * target * 3) as usize];

    // Билинейная ресайз-вставка. Для каждой строки назначения считаем
    // источник один раз — этого достаточно по качеству для int8 NPU.
    for dy in 0..new_h {
        let sy = ((dy as f32 + 0.5) / scale - 0.5).max(0.0);
        let sy0 = (sy as u32).min(h - 1);
        let fy = sy - sy0 as f32;
        let sy1 = (sy0 + 1).min(h - 1);
        for dx in 0..new_w {
            let sx = ((dx as f32 + 0.5) / scale - 0.5).max(0.0);
            let sx0 = (sx as u32).min(w - 1);
            let fx = sx - sx0 as f32;
            let sx1 = (sx0 + 1).min(w - 1);
            let dst = ((pad_y.max(0) as u32 + dy) * target + (pad_x.max(0) as u32 + dx)) as usize * 3;
            let s00 = (sy0 * w + sx0) as usize * 3;
            let s01 = (sy0 * w + sx1) as usize * 3;
            let s10 = (sy1 * w + sx0) as usize * 3;
            let s11 = (sy1 * w + sx1) as usize * 3;
            for c in 0..3 {
                let top = rgb[s00 + c] as f32 * (1.0 - fx) + rgb[s01 + c] as f32 * fx;
                let bot = rgb[s10 + c] as f32 * (1.0 - fx) + rgb[s11 + c] as f32 * fx;
                out[dst + c] = (top * (1.0 - fy) + bot * fy).round().clamp(0.0, 255.0) as u8;
            }
        }
    }

    (
        out,
        LetterboxParams {
            scale,
            pad_x: pad_x as f32,
            pad_y: pad_y as f32,
            target,
        },
    )
}

/// Конфигурация декодера.
#[derive(Debug, Clone)]
pub struct DecoderConfig {
    /// Порог уверенности объекта.
    pub conf_threshold: f32,
    /// Порог NMS (IoU).
    pub nms_threshold: f32,
    /// Имена классов (по индексу; лишние игнорируются, недостающие — "class_N").
    pub class_names: Vec<String>,
    /// Считать классы логитами и применить sigmoid (layout SingleHead).
    /// Для YoloBranches-6 автодетект выставит false.
    pub sigmoid_classes: bool,
}

impl Default for DecoderConfig {
    fn default() -> Self {
        Self {
            conf_threshold: 0.45,
            nms_threshold: 0.45,
            class_names: Vec::new(),
            sigmoid_classes: false,
        }
    }
}

/// Найденный layout выходов модели.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputLayout {
    /// 6 (или 3) выходов: пары (box_dfl, cls) на каждую ветку.
    YoloBranches,
    /// 1 выход [1, 4+Nc, A].
    SingleHead,
}

/// Определить layout по формам выходов (dims каждого выхода).
pub fn detect_layout(output_dims: &[Vec<u32>]) -> Option<OutputLayout> {
    if output_dims.len() == 1 && output_dims[0].len() == 3 {
        return Some(OutputLayout::SingleHead);
    }
    if output_dims.len() >= 6 {
        return Some(OutputLayout::YoloBranches);
    }
    None
}

/// Декодер YOLOv8-выходов RKNN.
pub struct YoloDecoder {
    pub config: DecoderConfig,
    pub layout: OutputLayout,
    /// Число классов, выведенное из формы выходов.
    pub num_classes: usize,
    /// Для SingleHead: (rows = 4+Nc, anchors).
    #[allow(dead_code)] // хранится для диагностики layout
    single_shape: (usize, usize),
}

impl YoloDecoder {
    /// Одно-выходная модель [1, 4+Nc, anchors]: int8-квантование такого
    /// тензора схлопывает cls-логиты (conf=0.5 артефакт, ADR-014).
    /// Фича отключена до появления 9-веточного экспорта.
    pub fn is_single_head(&self) -> bool {
        matches!(self.layout, OutputLayout::SingleHead)
    }

    /// Создать декодер по формам выходов модели.
    pub fn from_output_dims(output_dims: &[Vec<u32>], config: DecoderConfig) -> Option<Self> {
        let layout = detect_layout(output_dims)?;
        let num_classes = match layout {
            OutputLayout::SingleHead => {
                match single_head_shape(&output_dims[0]) {
                    Some((rows, _anchors)) => rows - 4,
                    None => return None,
                }
            }
            OutputLayout::YoloBranches => {
                // cls-выходы — нечётные индексы: [1, Nc, Gh, Gw].
                let dims = output_dims.get(1)?;
                if dims.len() == 4 {
                    dims[1] as usize
                } else {
                    return None;
                }
            }
        };
        let mut config = config;
        let single_shape = match layout {
            OutputLayout::SingleHead => single_head_shape(&output_dims[0])?,
            OutputLayout::YoloBranches => (0, 0),
        };
        if layout == OutputLayout::SingleHead {
            config.sigmoid_classes = true;
        }
        Some(Self {
            config,
            layout,
            num_classes,
            single_shape,
        })
    }

    /// Декодировать выходы модели в детекции в координатах исходного кадра.
    ///
    /// * `outputs` — по Vec<f32> на выход модели.
    /// * `lb` — параметры letterbox, применённого к кадру.
    /// * `orig_w/orig_h` — размер исходного кадра.
    /// * `frame_seq` — номер кадра (для телеметрии).
    pub fn decode(
        &self,
        outputs: &[Vec<f32>],
        output_dims: &[Vec<u32>],
        lb: &LetterboxParams,
        orig_w: u32,
        orig_h: u32,
        frame_seq: u64,
    ) -> Vec<Detection> {
        let now_ms = chrono_now_ms();
        let mut cands: Vec<(BBox, u32, f32)> = match self.layout {
            OutputLayout::SingleHead => {
                self.decode_single(&outputs[0], output_dims[0].clone(), lb)
            }
            OutputLayout::YoloBranches => self.decode_branches(outputs, output_dims, lb),
        };

        // Порог + валидность бокса.
        cands.retain(|(_, _, s)| {
            *s >= self.config.conf_threshold
        });

        // NMS по классам (жадный).
        let mut result: Vec<Detection> = Vec::new();
        let mut suppressed = vec![false; cands.len()];
        // Сортировка по убыванию score.
        let mut order: Vec<usize> = (0..cands.len()).collect();
        order.sort_by(|&a, &b| {
            cands[b].2.partial_cmp(&cands[a].2).unwrap_or(std::cmp::Ordering::Equal)
        });

        for &i in &order {
            if suppressed[i] {
                continue;
            }
            let (bbox, class_id, score) = &cands[i];
            for &j in &order {
                if j == i || suppressed[j] {
                    continue;
                }
                if cands[j].1 != *class_id {
                    continue;
                }
                if bbox.iou(&cands[j].0) > self.config.nms_threshold {
                    suppressed[j] = true;
                }
            }
            let mut b = *bbox;
            b.clamp_to(orig_w, orig_h);
            if b.w < 1.0 || b.h < 1.0 {
                continue;
            }
            result.push(Detection {
                bbox: b,
                class_id: *class_id,
                class_name: self.class_name(*class_id),
                confidence: *score,
                frame_seq,
                detected_at_ms: now_ms,
            });
        }
        result
    }

    fn class_name(&self, id: u32) -> String {
        self.config
            .class_names
            .get(id as usize)
            .cloned()
            .unwrap_or_else(|| format!("class_{id}"))
    }

    /// Layout SingleHead: [1, 4+Nc, A], координаты в пикселях target-пространства.
    fn decode_single(
        &self,
        out: &[f32],
        dims: Vec<u32>,
        lb: &LetterboxParams,
    ) -> Vec<(BBox, u32, f32)> {
        // rows = 4+Nc, anchors = A
        let (rows, anchors) = match single_head_shape(&dims) {
            Some(s) => s,
            None => return Vec::new(),
        };
        if out.len() < rows * anchors {
            return Vec::new();
        }
        let nc = rows - 4;
        let mut cands = Vec::new();
        for a in 0..anchors {
            let cx = out[a];
            let cy = out[anchors + a];
            let w = out[2 * anchors + a];
            let h = out[3 * anchors + a];
            let mut best_id = 0usize;
            let mut best = f32::NEG_INFINITY;
            for c in 0..nc {
                let raw = out[(4 + c) * anchors + a];
                let s = if self.config.sigmoid_classes {
                    sigmoid(raw)
                } else {
                    raw
                };
                if s > best {
                    best = s;
                    best_id = c;
                }
            }
            let conf = best.clamp(0.0, 1.0);
            if !conf.is_finite() || w <= 0.0 || h <= 0.0 || !cx.is_finite() || !cy.is_finite() {
                continue;
            }
            let (ox, oy) = lb.unproject_xy(cx, cy);
            let bw = w / lb.scale;
            let bh = h / lb.scale;
            cands.push((
                BBox::new(ox - bw * 0.5, oy - bh * 0.5, bw, bh),
                best_id as u32,
                conf,
            ));
        }
        cands
    }

    /// Layout YoloBranches: пары (box [1,64,Gh,Gw], cls [1,Nc,Gh,Gw]) × 3 ветки.
    /// Порт yolov8_utils.py (dfl/box_process/post_process).
    fn decode_branches(
        &self,
        outputs: &[Vec<f32>],
        output_dims: &[Vec<u32>],
        lb: &LetterboxParams,
    ) -> Vec<(BBox, u32, f32)> {
        let branches = 3usize;
        let pairs = outputs.len() / branches;
        let mut cands = Vec::new();
        for i in 0..branches {
            let box_out = &outputs[pairs * i];
            let cls_out = &outputs[pairs * i + 1];
            let box_dims = &output_dims[pairs * i];
            let cls_dims = &output_dims[pairs * i + 1];
            if box_dims.len() != 4 || cls_dims.len() != 4 {
                continue;
            }
            // Форма NCHW [1, C, Gh, Gw]: C — dims[1] (у box=64 канала DFL,
            // у cls=Nc классов; H,W — dims[2],dims[3]).
            let (_, _bc, gh, gw) = (box_dims[0], box_dims[1], box_dims[2], box_dims[3]);
            let nc = cls_dims[1];
            let img_w = lb.target;
            let img_h = lb.target;
            let stride_h = (img_h / gh).max(1) as f32;
            let stride_w = (img_w / gw).max(1) as f32;

            for gy in 0..gh as usize {
                for gx in 0..gw as usize {
                    // Индекс якоря в CHW-плоскости.
                    let spatial = gy * gw as usize + gx;
                    // === Классы ===
                    let mut best_id = 0usize;
                    let mut best = f32::NEG_INFINITY;
                    for c in 0..nc as usize {
                        let v = cls_out[c * gh as usize * gw as usize + spatial];
                        let s = if self.config.sigmoid_classes {
                            sigmoid(v)
                        } else {
                            v
                        };
                        if s > best {
                            best = s;
                            best_id = c;
                        }
                    }
                    let conf = best.clamp(0.0, 1.0);
                    if !conf.is_finite() {
                        continue;
                    }
                    // === DFL по 4 сторонам, 64 канала = 4 × 16 бинов ===
                    let (mut l, mut t, mut r, mut b) = (0f32, 0f32, 0f32, 0f32);
                    let gh_w = gh as usize * gw as usize;
                    for (side_idx, acc) in [&mut l, &mut t, &mut r, &mut b].iter_mut().enumerate() {
                        let ch_base = side_idx * 16 * gh_w + spatial;
                        // softmax по 16 бинам + матожидание
                        let mut m = f32::NEG_INFINITY;
                        let mut exps = [0f32; 16];
                        for k in 0..16 {
                            let v = box_out[ch_base + k * gh_w];
                            exps[k] = v;
                            if v > m {
                                m = v;
                            }
                        }
                        let mut sum = 0f32;
                        for e in exps.iter_mut() {
                            *e = (*e - m).exp();
                            sum += *e;
                        }
                        let mut e_val = 0f32;
                        for (k, e) in exps.iter().enumerate() {
                            e_val += *e / sum * k as f32;
                        }
                        **acc = e_val;
                    }
                    // grid + 0.5 ∓ dfl, в stride-пикселях
                    let gx_f = gx as f32;
                    let gy_f = gy as f32;
                    let x1 = (gx_f + 0.5 - l) * stride_w;
                    let y1 = (gy_f + 0.5 - t) * stride_h;
                    let x2 = (gx_f + 0.5 + r) * stride_w;
                    let y2 = (gy_f + 0.5 + b) * stride_h;
                    let (ox1, oy1) = lb.unproject_xy(x1, y1);
                    let (ox2, oy2) = lb.unproject_xy(x2, y2);
                    let bw = ox2 - ox1;
                    let bh = oy2 - oy1;
                    if bw <= 0.0 || bh <= 0.0 {
                        continue;
                    }
                    cands.push((BBox::new(ox1, oy1, bw, bh), best_id as u32, conf));
                }
            }
        }
        cands
    }
}

/// Форма [1, rows, anchors] single-head выхода. Приоритет — точная
/// интерпретация N-first; иначе сортировочная эвристика (A >> 4+Nc).
fn single_head_shape(dims: &[u32]) -> Option<(usize, usize)> {
    if dims.len() != 3 {
        return None;
    }
    if dims[0] == 1 && dims[1] >= 5 && dims[2] >= 1 {
        return Some((dims[1] as usize, dims[2] as usize));
    }
    let mut v: Vec<u32> = dims.to_vec();
    v.sort_unstable_by(|a, b| b.cmp(a));
    if v[2] == 1 && v[1] >= 5 {
        Some((v[1] as usize, v[0] as usize))
    } else {
        None
    }
}

#[inline]
fn sigmoid(x: f32) -> f32 {
    if x >= 0.0 {
        1.0 / (1.0 + (-x).exp())
    } else {
        let e = x.exp();
        e / (1.0 + e)
    }
}

pub fn chrono_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}


/// Геометрия тайла для zoom-инференса мелких целей (R2 из refvideo/RESULTS,
/// ADR-022): 2×2 перекрывающихся тайла, ~10 % перекрытия — цель 7×4 px
/// превращается в ~14×8 px на входе модели.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tile {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// Тайлы 2×2 с перекрытием ~10 % по каждой оси (для 640×480:
/// 352×264 с шагом 288/216). Кадры меньше 2×32 — один тайл на весь кадр.
pub fn tiles_2x2(w: u32, h: u32) -> Vec<Tile> {
    if w < 64 || h < 64 {
        return vec![Tile { x: 0, y: 0, w, h }];
    }
    let tile_w = ((w as u32 * 11) / 20).max(32); // ~55 %
    let tile_h = ((h as u32 * 11) / 20).max(32);
    let tile_w = tile_w.min(w);
    let tile_h = tile_h.min(h);
    let stride_x = w - tile_w;
    let stride_y = h - tile_h;
    let mut out = Vec::with_capacity(4);
    for ty in [0, stride_y] {
        for tx in [0, stride_x] {
            out.push(Tile { x: tx, y: ty, w: tile_w, h: tile_h });
        }
    }
    out
}

/// Кроп под-прямоугольника из interleaved RGB24.
pub fn crop_rgb24(rgb: &[u8], w: u32, t: Tile) -> Vec<u8> {
    let mut out = Vec::with_capacity((t.w * t.h * 3) as usize);
    for row in 0..t.h {
        let src = (((t.y + row) * w + t.x) as usize) * 3;
        let src = src.min(rgb.len().saturating_sub((t.w as usize) * 3));
        out.extend_from_slice(&rgb[src..src + (t.w as usize) * 3]);
    }
    out
}

/// Сдвиг детекции из координат тайла в координаты кадра.
pub fn det_to_frame(mut d: common::Detection, t: Tile) -> common::Detection {
    d.bbox.x += t.x as f32;
    d.bbox.y += t.y as f32;
    d
}

/// NMS по классам и IoU; вход сортируется по уверенности.
pub fn nms_merge(
    dets: Vec<common::Detection>,
    iou_th: f32,
) -> Vec<common::Detection> {
    let mut dets = dets;
    dets.sort_by(|a, b| b.confidence.partial_cmp(&a.confidence).unwrap_or(std::cmp::Ordering::Equal));
    let mut kept: Vec<common::Detection> = Vec::new();
    'outer: for d in dets {
        for k in &kept {
            if k.class_id == d.class_id && k.bbox.iou(&d.bbox) > iou_th {
                continue 'outer;
            }
        }
        kept.push(d);
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lb_640() -> LetterboxParams {
        LetterboxParams {
            scale: 1.0,
            pad_x: 0.0,
            pad_y: 80.0, // 640x480 → 640x640: полосы по 80 сверху/снизу
            target: 640,
        }
    }

    #[test]
    fn layout_detection() {
        let single = vec![vec![1, 84, 8400]];
        assert_eq!(detect_layout(&single), Some(OutputLayout::SingleHead));
        let branches = vec![
            vec![1, 64, 80, 80],
            vec![1, 5, 80, 80],
            vec![1, 64, 40, 40],
            vec![1, 5, 40, 40],
            vec![1, 64, 20, 20],
            vec![1, 5, 20, 20],
        ];
        assert_eq!(detect_layout(&branches), Some(OutputLayout::YoloBranches));
    }

    #[test]
    fn single_head_decode_center_box() {
        // Якорь с центром (320, 320), размером 100x50, класс 2, логит 4 → sigmoid ≈ 0.982.
        let rows = 6usize; // 4 + 2 класса
        let anchors = 1usize;
        let mut out = vec![0f32; rows * anchors];
        out[0] = 320.0;
        out[1] = 320.0;
        out[2] = 100.0;
        out[3] = 50.0;
        out[4] = -10.0;
        out[5] = 4.0;
        let dec = YoloDecoder::from_output_dims(
            &[vec![1, 6, 1]],
            DecoderConfig {
                conf_threshold: 0.5,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(dec.num_classes, 2);
        let dets = dec.decode(&[out], &[vec![1, 6, 1]], &lb_640(), 640, 480, 0);
        assert_eq!(dets.len(), 1);
        let d = &dets[0];
        assert_eq!(d.class_id, 1);
        // y-центр 320 в letterbox → исходный y = 320-80 = 240
        let (cx, cy) = d.bbox.center();
        assert!((cx - 320.0).abs() < 1.0, "cx={cx}");
        assert!((cy - 240.0).abs() < 1.0, "cy={cy}");
        assert!((d.bbox.w - 100.0).abs() < 1.0);
        assert!((d.bbox.h - 50.0).abs() < 1.0);
        assert!(d.confidence > 0.98);
    }

    #[test]
    fn nms_suppresses_overlap() {
        // Layout [1, rows=5, anchors=2]: f[row * anchors + a], rows 0..3 — боксы.
        let mut out = vec![0f32; 5 * 2];
        // Якорь 0: центр (100,100), 50x50, логит класса 0 = 5
        out[0] = 100.0; // cx
        out[2] = 100.0; // cy
        out[4] = 50.0; // w
        out[6] = 50.0; // h
        out[8] = 5.0; // cls0
        // Якорь 1: почти тот же бокс, логит ниже
        out[1] = 102.0;
        out[3] = 100.0;
        out[5] = 50.0;
        out[7] = 50.0;
        out[9] = 4.0;
        let dec = YoloDecoder::from_output_dims(
            &[vec![1, 5, 2]],
            DecoderConfig {
                conf_threshold: 0.5,
                nms_threshold: 0.45,
                ..Default::default()
            },
        )
        .unwrap();
        let dets = dec.decode(&[out], &[vec![1, 5, 2]], &lb_640(), 640, 480, 0);
        assert_eq!(dets.len(), 1, "NMS должен оставить один бокс");
    }

    #[test]
    fn letterbox_square_no_pad() {
        let rgb = vec![200u8; 64 * 64 * 3];
        let (out, lb) = letterbox_rgb24(&rgb, 64, 64, 32);
        assert_eq!(out.len(), 32 * 32 * 3);
        assert!((lb.scale - 0.5).abs() < 1e-6);
        assert_eq!(lb.pad_x, 0.0);
        assert_eq!(lb.pad_y, 0.0);
        // Все пиксели из источника (без заполнения 114).
        assert!(out.iter().all(|&v| v == 200));
    }
    #[test]
    fn tiles_cover_frame_with_overlap() {
        let ts = tiles_2x2(640, 480);
        assert_eq!(ts.len(), 4);
        // правый нижний угол покрыт
        let br = ts.iter().map(|t| (t.x + t.w, t.y + t.h)).max().unwrap();
        assert_eq!(br, (640, 480));
        // перекрытие ~10 % по обеим осям (тип 352 -> 64 px, 264 -> 48 px)
        assert_eq!(ts[0].w - (ts[1].x - ts[0].x), 64);
        assert_eq!(ts[0].h - (ts[2].y - ts[0].y), 48);
        // мелкий кадр — один тайл
        assert_eq!(tiles_2x2(32, 32).len(), 1);
    }

    #[test]
    fn crop_and_remap_roundtrip() {
        // кадр 64x48, тайл 36x28 с шагом 28/20; белый квадрат в правом нижнем
        let (w, h) = (64u32, 48u32);
        let mut rgb = vec![0u8; (w * h * 3) as usize];
        for y in 40..44 {
            for x in 56..60 {
                for c in 0..3 {
                    rgb[((y * w + x) * 3 + c) as usize] = 255;
                }
            }
        }
        let ts = tiles_2x2(w, h);
        let t = *ts.last().unwrap(); // правый нижний тайл
        let crop = crop_rgb24(&rgb, w, t);
        assert_eq!(crop.len(), (t.w * t.h * 3) as usize);
        // квадрат (56,40)-(60,44) внутри тайла -> локальные (56-t.x, 40-t.y)
        let lx = (56 - t.x) as usize;
        let ly = (40 - t.y) as usize;
        let cx = (ly * t.w as usize + lx) * 3;
        assert_eq!(&crop[cx..cx + 3], &[255, 255, 255]);
        // детекция из тайла -> координаты кадра
        let d = common::Detection {
            bbox: BBox::new(lx as f32, ly as f32, 4.0, 4.0),
            class_id: 0,
            class_name: "t".into(),
            confidence: 0.9,
            frame_seq: 1,
            detected_at_ms: 0,
        };
        let fd = det_to_frame(d, t);
        assert_eq!((fd.bbox.x as u32, fd.bbox.y as u32), (56, 40));
    }

    #[test]
    fn nms_merge_dedups_overlap() {
        let mk = |x: f32, c: f32| common::Detection {
            bbox: BBox::new(x, 0.0, 10.0, 10.0),
            class_id: 0,
            class_name: "t".into(),
            confidence: c,
            frame_seq: 1,
            detected_at_ms: 0,
        };
        let merged = nms_merge(vec![mk(0.0, 0.5), mk(1.0, 0.9), mk(60.0, 0.3)], 0.45);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].confidence, 0.9); // сильнейший выжил
        let merged2 = nms_merge(vec![mk(0.0, 0.5), mk(60.0, 0.9)], 0.45);
        assert_eq!(merged2.len(), 2); // дальние не сливаются
    }
}

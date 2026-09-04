//! Общие типы данных synergyAutotargeting: кадры, форматы, боксы, детекции.
//!
//! Сознательно максимально плоские и сериализуемые — используются всеми
//! крейтами workspace и попадают в телеметрию (JSONL).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Пиксельный формат кадра.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PixelFormat {
    /// Сжатый JPEG-кадр (как отдаёт UVC-камера по USB для экономии полосы).
    Mjpeg,
    /// YUYV 4:2:2 packed.
    Yuyv,
    /// NV12 (YUV 4:2:0 semi-planar, родной для Rockchip).
    Nv12,
    /// RGB24 packed, 3 байта на пиксель.
    Rgb24,
    /// Raw Bayer GRBG 8 бит (Sony PS Eye / ov534): 1 байт на пиксель.
    BayerGrbg,
}

impl PixelFormat {
    pub fn bytes_per_pixel(&self) -> usize {
        match self {
            PixelFormat::Mjpeg => 0, // сжатый
            PixelFormat::Yuyv => 2,
            PixelFormat::Nv12 => 3, // 1.5 на пиксель, но удобнее 3/2 учитывать отдельно
            PixelFormat::Rgb24 => 3,
            PixelFormat::BayerGrbg => 1,
        }
    }
}

/// Метаданные кадра.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrameMetadata {
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
    pub captured_at: DateTime<Utc>,
    pub seq: u64,
}

/// Кадр видео: сырые байты + метаданные.
#[derive(Debug, Clone)]
pub struct Frame {
    pub data: Vec<u8>,
    pub metadata: FrameMetadata,
}

impl Frame {
    pub fn new(data: Vec<u8>, width: u32, height: u32, format: PixelFormat, seq: u64) -> Self {
        Self {
            data,
            metadata: FrameMetadata {
                width,
                height,
                format,
                captured_at: Utc::now(),
                seq,
            },
        }
    }
}

/// Прямоугольник в пикселях исходного кадра (x,y — левый верхний угол).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BBox {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl BBox {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    pub fn center(&self) -> (f32, f32) {
        (self.x + self.w * 0.5, self.y + self.h * 0.5)
    }

    pub fn area(&self) -> f32 {
        self.w.max(0.0) * self.h.max(0.0)
    }

    pub fn x1(&self) -> f32 {
        self.x
    }
    pub fn y1(&self) -> f32 {
        self.y
    }
    pub fn x2(&self) -> f32 {
        self.x + self.w
    }
    pub fn y2(&self) -> f32 {
        self.y + self.h
    }

    /// Ограничить бокс границами кадра.
    pub fn clamp_to(&mut self, width: u32, height: u32) {
        let w = width as f32;
        let h = height as f32;
        let x1 = self.x.clamp(0.0, w);
        let y1 = self.y.clamp(0.0, h);
        let x2 = self.x2().clamp(x1, w);
        let y2 = self.y2().clamp(y1, h);
        self.x = x1;
        self.y = y1;
        self.w = x2 - x1;
        self.h = y2 - y1;
    }

    /// Intersection over Union с другим боксом.
    pub fn iou(&self, other: &BBox) -> f32 {
        let ix1 = self.x1().max(other.x1());
        let iy1 = self.y1().max(other.y1());
        let ix2 = self.x2().min(other.x2());
        let iy2 = self.y2().min(other.y2());
        let iw = (ix2 - ix1).max(0.0);
        let ih = (iy2 - iy1).max(0.0);
        let inter = iw * ih;
        let union = self.area() + other.area() - inter;
        if union > 0.0 {
            inter / union
        } else {
            0.0
        }
    }

    /// Бокс как целочисленный (x, y, w, h) для отрисовки.
    pub fn as_ints(&self) -> (i32, i32, i32, i32) {
        (
            self.x.round() as i32,
            self.y.round() as i32,
            self.w.round() as i32,
            self.h.round() as i32,
        )
    }
}

/// Одна детекция от нейросети-детектора.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Detection {
    pub bbox: BBox,
    pub class_id: u32,
    pub class_name: String,
    pub confidence: f32,
    pub frame_seq: u64,
    /// Unix-время детекции, мс.
    pub detected_at_ms: u64,
}

/// Конфигурация видеоисточника (из TOML).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoConfig {
    pub device: String,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// "mjpeg" | "yuyv" | "nv12" | "rgb24" | "grbg" (raw Bayer PS Eye)
    pub format: String,
    pub queue_depth: usize,
}

impl Default for VideoConfig {
    fn default() -> Self {
        Self {
            device: "/dev/video0".into(),
            width: 640,
            height: 480,
            fps: 30,
            format: "mjpeg".into(),
            queue_depth: 4,
        }
    }
}

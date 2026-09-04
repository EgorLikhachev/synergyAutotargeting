//! Захват видео — V4L2 direct ioctl (порт из Autotargeting, обкатан на Arducam)
//! + конверсия форматов (MJPEG/YUYV/NV12 → RGB24).
//!
//! ## Происхождение
//! - `v4l2_direct.rs` — дословный перенос из Autotargeting `crates/video-capture`
//!   (ADR D-11: путь через `v4l`-crate давал 21 FPS, прямой ioctl — 90-100 FPS).
//! - `convert.rs` — дословный перенос оттуда же (jpeg-decoder, pure Rust).

pub mod convert;
pub mod traits;

#[cfg(target_os = "linux")]
pub mod v4l2_direct;

pub use convert::{
    convert_to, decode_mjpeg_to_rgb, nv12_to_rgb24, yuyv_to_rgb24, ConversionError,
    ConversionResult,
};
pub use traits::{VideoCaptureError, VideoResult, VideoSource};

#[cfg(target_os = "linux")]
pub use v4l2_direct::V4l2DirectSource;

pub use common::{Frame, FrameMetadata, PixelFormat};

/// Конфигурация источника видео (совместима с common::VideoConfig).
#[derive(Debug, Clone)]
pub struct VideoSourceConfig {
    pub device: String,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub format: PixelFormat,
    pub queue_depth: usize,
}

impl VideoSourceConfig {
    pub fn from_common(cfg: &common::VideoConfig) -> Self {
        let format = match cfg.format.as_str() {
            "nv12" => PixelFormat::Nv12,
            "yuyv" => PixelFormat::Yuyv,
            "rgb24" => PixelFormat::Rgb24,
            "mjpeg" => PixelFormat::Mjpeg,
            "grbg" => PixelFormat::BayerGrbg,
            other => {
                tracing::warn!(format = other, "неизвестный формат пикселей, беру MJPEG");
                PixelFormat::Mjpeg
            }
        };
        Self {
            device: cfg.device.clone(),
            width: cfg.width,
            height: cfg.height,
            fps: cfg.fps,
            format,
            queue_depth: cfg.queue_depth,
        }
    }
}

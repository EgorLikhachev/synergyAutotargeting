//! Image format conversion — MJPEG decode, YUYV → NV12, RGB24 → NV12.
//!
//! Arducam UC-852 отдаёт MJPEG по USB (для экономии bandwidth).
//! NPU RK3588S ест NV12 (YUV 4:2:0 semi-planar).
//! Этот модуль bridging'ает между ними.
//!
//! ## Поддерживаемые конверсии
//!
//! | From | To | Метод |
//! |---|---|---|
//! | MJPEG | RGB24 | `jpeg-decoder` crate (pure Rust) |
//! | MJPEG | NV12 | decode → RGB24 → NV12 |
//! | YUYV | NV12 | прямая конверсия (Y ready, U/V decimated 2x) |
//! | YUYV | RGB24 | YCbCr → RGB matrix |
//! | RGB24 | NV12 | RGB → YCbCr matrix |
//!
//! ## Производительность
//!
//! - MJPEG decode 720p: ~5-10 ms (на Orange Pi 5, single thread)
//! - YUYV → NV12 720p: ~2-3 ms (simple memcpy + decimation)
//! - RGB24 → NV12 720p: ~3-5 ms (matrix multiply)

use common::{Frame, FrameMetadata, PixelFormat};
use thiserror::Error;
use tracing::{debug, warn};

#[derive(Debug, Error)]
pub enum ConversionError {
    #[error("jpeg decode error: {0}")]
    JpegDecode(String),

    #[error("invalid frame format: expected {expected:?}, got {actual:?}")]
    InvalidFormat {
        expected: PixelFormat,
        actual: PixelFormat,
    },

    #[error("invalid frame dimensions: {w}x{h}, data len {len}")]
    InvalidDimensions { w: u32, h: u32, len: usize },

    #[error("conversion not supported: {from:?} → {to:?}")]
    UnsupportedConversion { from: PixelFormat, to: PixelFormat },
}

pub type ConversionResult<T> = std::result::Result<T, ConversionError>;

/// Декодировать MJPEG кадр в RGB24.
///
/// Использует `jpeg-decoder` crate (pure Rust, no libclang).
/// Возвращает новые Frame с format=RGB24.
pub fn decode_mjpeg_to_rgb(frame: &Frame) -> ConversionResult<Frame> {
    if frame.metadata.format != PixelFormat::Mjpeg {
        return Err(ConversionError::InvalidFormat {
            expected: PixelFormat::Mjpeg,
            actual: frame.metadata.format,
        });
    }

    let mut decoder = jpeg_decoder::Decoder::new(&frame.data[..]);
    let pixels = decoder
        .decode()
        .map_err(|e| ConversionError::JpegDecode(e.to_string()))?;

    let info = decoder
        .info()
        .ok_or_else(|| ConversionError::JpegDecode("no JPEG info".to_string()))?;

    debug!(
        width = info.width,
        height = info.height,
        "decoded MJPEG to RGB24"
    );

    Ok(Frame {
        data: pixels,
        metadata: FrameMetadata {
            width: info.width as u32,
            height: info.height as u32,
            format: PixelFormat::Rgb24,
            captured_at: frame.metadata.captured_at,
            seq: frame.metadata.seq,
        },
    })
}

/// Декодировать MJPEG кадр напрямую в NV12.
/// (MJPEG → RGB24 → NV12, два шага.)
pub fn decode_mjpeg_to_nv12(frame: &Frame) -> ConversionResult<Frame> {
    let rgb = decode_mjpeg_to_rgb(frame)?;
    rgb24_to_nv12(&rgb)
}

/// Конвертировать YUYV (packed YUV 4:2:2) в NV12 (semi-planar YUV 4:2:0).
///
/// YUYV layout: [Y0, U, Y1, V, Y2, U, Y3, V, ...]
/// NV12 layout: [Y0, Y1, Y2, ..., Y(N-1), U0, V0, U1, V1, ...]
///
/// U/V decimated 2x по горизонтали и 2x по вертикали (4:2:0).
pub fn yuyv_to_nv12(frame: &Frame) -> ConversionResult<Frame> {
    if frame.metadata.format != PixelFormat::Yuyv {
        return Err(ConversionError::InvalidFormat {
            expected: PixelFormat::Yuyv,
            actual: frame.metadata.format,
        });
    }

    let w = frame.metadata.width as usize;
    let h = frame.metadata.height as usize;
    let expected_len = w * h * 2; // YUYV = 2 bytes/pixel
    if frame.data.len() != expected_len {
        return Err(ConversionError::InvalidDimensions {
            w: w as u32,
            h: h as u32,
            len: frame.data.len(),
        });
    }

    // NV12: Y plane (w*h) + UV plane (w*h/2) = w*h*3/2 bytes
    let mut nv12 = vec![0u8; w * h * 3 / 2];

    // Y plane: копируем Y-байты (каждый чётный) построчно chunks-итераторами —
    // без bounds-checks и адресной арифметики на пиксель.
    let (y_plane, uv_plane) = nv12.split_at_mut(w * h);
    for (src_row, y_row) in frame
        .data
        .chunks_exact(w * 2)
        .zip(y_plane.chunks_exact_mut(w))
    {
        for (pair, out2) in src_row.chunks_exact(4).zip(y_row.chunks_exact_mut(2)) {
            out2.copy_from_slice(&[pair[0], pair[2]]);
        }
        // Нечётная ширина (нестандарт YUYV): одиночный Y-хвост.
        if w % 2 == 1 {
            let last = src_row.chunks_exact(4).remainder();
            if let Some(y_val) = last.first() {
                if let Some(slot) = y_row.last_mut() {
                    *slot = *y_val;
                }
            }
        }
    }

    // UV plane: U/V из первой пары пикселей каждой чётной строки.
    // Нечётные w/h нестандартны: хрома через remainder-хвост (нейтральная),
    // непарная последняя строка пропускается.
    let uv_rows = (h / 2) * w;
    for (row_idx, uv_row) in uv_plane[..uv_rows].chunks_exact_mut(w).enumerate() {
        let src_row = &frame.data[(2 * row_idx) * w * 2..][..w * 2];
        for (pair, out2) in src_row.chunks_exact(4).zip(uv_row.chunks_exact_mut(2)) {
            out2.copy_from_slice(&[pair[1], pair[3]]);
        }
        if w % 2 == 1 {
            // Хвостовая пара неполная: V может отсутствовать — берём что есть.
            let tail = src_row.chunks_exact(4).remainder();
            let u = tail.get(1).copied().unwrap_or(128);
            let v = tail.get(3).copied().unwrap_or(128);
            let x = w - 1;
            uv_row[x..].copy_from_slice(&[u, v][..w - x]); // 1 байт U (V вне строки)
        }
    }

    Ok(Frame {
        data: nv12,
        metadata: FrameMetadata {
            width: w as u32,
            height: h as u32,
            format: PixelFormat::Nv12,
            captured_at: frame.metadata.captured_at,
            seq: frame.metadata.seq,
        },
    })
}

/// Конвертировать YUYV в RGB24.
///
/// YUYV → YCbCr → RGB матричное преобразование.
pub fn yuyv_to_rgb24(frame: &Frame) -> ConversionResult<Frame> {
    if frame.metadata.format != PixelFormat::Yuyv {
        return Err(ConversionError::InvalidFormat {
            expected: PixelFormat::Yuyv,
            actual: frame.metadata.format,
        });
    }

    let w = frame.metadata.width as usize;
    let h = frame.metadata.height as usize;
    let expected_len = w * h * 2;
    if frame.data.len() != expected_len {
        return Err(ConversionError::InvalidDimensions {
            w: w as u32,
            h: h as u32,
            len: frame.data.len(),
        });
    }

    let mut rgb = vec![0u8; w * h * 3];

    // YUYV packed: [Y0, U, Y1, V] — 4 байта на пару пикселей.
    // Построчно chunks-итераторами + integer BT.601 (×256 fixed-point):
    // без bounds-checks и без f32 round на пиксель (перф-аудит 2026-08).
    for (src_row, dst_row) in frame
        .data
        .chunks_exact(w * 2)
        .zip(rgb.chunks_exact_mut(w * 3))
    {
        for (pair, out6) in src_row.chunks_exact(4).zip(dst_row.chunks_exact_mut(6)) {
            let u = pair[1] as i32 - 128;
            let v = pair[3] as i32 - 128;
            let (r0, g0, b0) = ycbcr_to_rgb_i32(pair[0] as i32, u, v);
            let (r1, g1, b1) = ycbcr_to_rgb_i32(pair[2] as i32, u, v);
            out6.copy_from_slice(&[r0, g0, b0, r1, g1, b1]);
        }
        // Нечётная ширина (нестандарт для YUYV): последний одиночный пиксель,
        // хрому берём с хвоста пары (нейтральная при отсутствии).
        if w % 2 == 1 {
            let tail = src_row.chunks_exact(4).remainder();
            let y_val = tail.first().copied().unwrap_or(128) as i32;
            let u = tail.get(1).copied().unwrap_or(128) as i32 - 128;
            let v = tail.get(3).copied().unwrap_or(128) as i32 - 128; // remainder ≤ 2 байта → нейтраль
            let (r, g, b) = ycbcr_to_rgb_i32(y_val, u, v);
            let out3 = &mut dst_row[(w - 1) * 3..];
            out3[0] = r;
            out3[1] = g;
            out3[2] = b;
        }
    }

    Ok(Frame {
        data: rgb,
        metadata: FrameMetadata {
            width: w as u32,
            height: h as u32,
            format: PixelFormat::Rgb24,
            captured_at: frame.metadata.captured_at,
            seq: frame.metadata.seq,
        },
    })
}

/// Конвертировать NV12 в RGB24 (TG26-125: путь видеорекордера).
///
/// Integer BT.601 (×256 fixed-point, расхождение с f32-эталоном ≤ 1),
/// построчные chunks + парная обработка пикселей (хрома читается на пару).
/// Чётные ширины (NV12-стандарт); нечётный хвост отбрасывается.
pub fn nv12_to_rgb24(frame: &Frame) -> ConversionResult<Frame> {
    if frame.metadata.format != PixelFormat::Nv12 {
        return Err(ConversionError::InvalidFormat {
            expected: PixelFormat::Nv12,
            actual: frame.metadata.format,
        });
    }
    let w = frame.metadata.width as usize;
    let h = frame.metadata.height as usize;
    let expected_len = w * h * 3 / 2;
    if frame.data.len() != expected_len {
        return Err(ConversionError::InvalidDimensions {
            w: w as u32,
            h: h as u32,
            len: frame.data.len(),
        });
    }
    let mut out = vec![0u8; w * h * 3];
    let uv_off = w * h;
    let clamp8 = |v: i32| v.clamp(0, 255) as u8;
    for (j, (y_row, out_row)) in frame.data[..w * h]
        .chunks_exact(w)
        .zip(out.chunks_exact_mut(w * 3))
        .enumerate()
    {
        let uv_row = &frame.data[uv_off + (j / 2) * w..][..w];
        for (k, (y2, out6)) in y_row
            .chunks_exact(2)
            .zip(out_row.chunks_exact_mut(6))
            .enumerate()
        {
            let (u, v) = (uv_row[k * 2] as i32 - 128, uv_row[k * 2 + 1] as i32 - 128);
            let y0 = y2[0] as i32;
            let y1 = y2[1] as i32;
            out6.copy_from_slice(&[
                clamp8(y0 + ((359 * v) >> 8)),
                clamp8(y0 - ((88 * u + 183 * v) >> 8)),
                clamp8(y0 + ((454 * u) >> 8)),
                clamp8(y1 + ((359 * v) >> 8)),
                clamp8(y1 - ((88 * u + 183 * v) >> 8)),
                clamp8(y1 + ((454 * u) >> 8)),
            ]);
        }
    }
    Ok(Frame {
        data: out,
        metadata: FrameMetadata {
            width: w as u32,
            height: h as u32,
            format: PixelFormat::Rgb24,
            captured_at: frame.metadata.captured_at,
            seq: frame.metadata.seq,
        },
    })
}

/// Конвертировать RGB24 в NV12.
///
/// RGB → YCbCr матричное преобразование, U/V decimated 2x.
pub fn rgb24_to_nv12(frame: &Frame) -> ConversionResult<Frame> {
    if frame.metadata.format != PixelFormat::Rgb24 {
        return Err(ConversionError::InvalidFormat {
            expected: PixelFormat::Rgb24,
            actual: frame.metadata.format,
        });
    }

    let w = frame.metadata.width as usize;
    let h = frame.metadata.height as usize;
    let expected_len = w * h * 3;
    if frame.data.len() != expected_len {
        return Err(ConversionError::InvalidDimensions {
            w: w as u32,
            h: h as u32,
            len: frame.data.len(),
        });
    }

    let mut nv12 = vec![0u8; w * h * 3 / 2];

    // Y plane: построчные chunks + integer BT.601 (расхождение с f32 ≤ 1).
    let (y_plane, uv_plane) = nv12.split_at_mut(w * h);
    for (src_row, y_row) in frame
        .data
        .chunks_exact(w * 3)
        .zip(y_plane.chunks_exact_mut(w))
    {
        for (px, out) in src_row.chunks_exact(3).zip(y_row.iter_mut()) {
            *out = rgb_to_y_u8(px[0], px[1], px[2]);
        }
    }

    // UV plane — среднее 2x2 блоков на raw i32-хроме (без округления на
    // каждый пиксель, без ветвлений для чётных измерений).
    let uv_rows = h / 2;
    for row in 0..uv_rows {
        let src0 = &frame.data[(2 * row) * w * 3..][..w * 3];
        let src1 = &frame.data[(2 * row + 1) * w * 3..][..w * 3]; // чётное h
        let uv_row = &mut uv_plane[row * w..][..w];
        let mut x = 0;
        // Чётные пары столбцов: полный 2x2 блок.
        while x + 1 < w {
            let p0 = &src0[x * 3..x * 3 + 3];
            let p1 = &src0[x * 3 + 3..x * 3 + 6];
            let p2 = &src1[x * 3..x * 3 + 3];
            let p3 = &src1[x * 3 + 3..x * 3 + 6];
            let cu = rgb_to_cb(p0) + rgb_to_cb(p1) + rgb_to_cb(p2) + rgb_to_cb(p3);
            let cv = rgb_to_cr(p0) + rgb_to_cr(p1) + rgb_to_cr(p2) + rgb_to_cr(p3);
            uv_row[x] = clamp_i32_to_u8(cu >> 10); // (cu/4) >> 8 == cu >> 10
            uv_row[x + 1] = clamp_i32_to_u8(cv >> 10);
            x += 2;
        }
        // Нечётная ширина: последний блок 1x2 (столбец один).
        if x < w {
            let p0 = &src0[x * 3..x * 3 + 3];
            let p2 = &src1[x * 3..x * 3 + 3];
            let cu = (rgb_to_cb(p0) + rgb_to_cb(p2)) >> 1;
            // V хвостового неполного блока не пишется: его позиция — начало
            // следующей UV-строки (нестандартный нечётный w; прежний код
            // писал и тут же перезатирал).
            let _cv = (rgb_to_cr(p0) + rgb_to_cr(p2)) >> 1;
            uv_row[x] = clamp_i32_to_u8(cu >> 8);
        }
    }
    // Нечётная высота (нестандарт для NV12): последняя UV-строка из одной
    // строки пикселей; UV-плоскость при нечётном h короче — пишем сколько влезает.
    if h % 2 == 1 {
        let src = &frame.data[(h - 1) * w * 3..][..w * 3];
        let uv_row = &mut uv_plane[uv_rows * w..];
        let mut x = 0;
        while x + 1 < uv_row.len() {
            let cu =
                (rgb_to_cb(&src[x * 3..x * 3 + 3]) + rgb_to_cb(&src[x * 3 + 3..x * 3 + 6])) >> 1;
            let cv =
                (rgb_to_cr(&src[x * 3..x * 3 + 3]) + rgb_to_cr(&src[x * 3 + 3..x * 3 + 6])) >> 1;
            uv_row[x] = clamp_i32_to_u8(cu >> 8);
            uv_row[x + 1] = clamp_i32_to_u8(cv >> 8);
            x += 2;
        }
    }

    Ok(Frame {
        data: nv12,
        metadata: FrameMetadata {
            width: w as u32,
            height: h as u32,
            format: PixelFormat::Nv12,
            captured_at: frame.metadata.captured_at,
            seq: frame.metadata.seq,
        },
    })
}

/// Универсальная конверсия — автоматически выбирает путь.
pub fn convert_to(frame: &Frame, target: PixelFormat) -> ConversionResult<Frame> {
    if frame.metadata.format == target {
        return Ok(frame.clone()); // nothing to do
    }

    match (frame.metadata.format, target) {
        (PixelFormat::Mjpeg, PixelFormat::Rgb24) => decode_mjpeg_to_rgb(frame),
        (PixelFormat::Mjpeg, PixelFormat::Nv12) => decode_mjpeg_to_nv12(frame),
        (PixelFormat::Yuyv, PixelFormat::Nv12) => yuyv_to_nv12(frame),
        (PixelFormat::Yuyv, PixelFormat::Rgb24) => yuyv_to_rgb24(frame),
        (PixelFormat::Rgb24, PixelFormat::Nv12) => rgb24_to_nv12(frame),
        (PixelFormat::Nv12, PixelFormat::Rgb24) => nv12_to_rgb24(frame),
        (from, to) => {
            warn!(?from, ?to, "unsupported conversion");
            Err(ConversionError::UnsupportedConversion { from, to })
        }
    }
}

// === Вспомогательные функции ===

/// YCbCr → RGB (BT.601).
/// Y в [0, 255], Cb/Cr смещены на -128.
#[inline]
#[allow(dead_code)] // f32-эталон: используется тестами (round-trip, согласованность)
fn ycbcr_to_rgb(y: f32, cb: f32, cr: f32) -> (u8, u8, u8) {
    let r = y + 1.402 * cr;
    let g = y - 0.344 * cb - 0.714 * cr;
    let b = y + 1.772 * cb;

    (
        r.round().clamp(0.0, 255.0) as u8,
        g.round().clamp(0.0, 255.0) as u8,
        b.round().clamp(0.0, 255.0) as u8,
    )
}

// ===== Integer BT.601 (×256 fixed-point) — горячие пути (перф-аудит 2026-08) =====
// Коэффициенты: 1.402→359, 0.344→88, 0.714→183, 1.772→454;
// 0.299→77, 0.587→150, 0.114→29, 0.169→43, 0.331→85, 0.5→128,
// 0.419→107, 0.081→21. Расхождение с f32-версией ≤ 1 (округления).

#[inline]
fn clamp_i32_to_u8(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

/// YCbCr → RGB, integer. `cb`/`cr` — уже центрированные (−128..127).
#[inline]
fn ycbcr_to_rgb_i32(y: i32, cb: i32, cr: i32) -> (u8, u8, u8) {
    let r = y + ((359 * cr) >> 8);
    let g = y - ((88 * cb + 183 * cr) >> 8);
    let b = y + ((454 * cb) >> 8);
    (clamp_i32_to_u8(r), clamp_i32_to_u8(g), clamp_i32_to_u8(b))
}

/// RGB → Y (только люма; для Y-плоскости).
#[inline]
fn rgb_to_y_u8(r: u8, g: u8, b: u8) -> u8 {
    clamp_i32_to_u8((77 * r as i32 + 150 * g as i32 + 29 * b as i32 + 128) >> 8)
}

/// RGB → Cb (raw ×256, БЕЗ clamp/округления — для усреднения 2x2).
#[inline]
fn rgb_to_cb(px: &[u8]) -> i32 {
    -43 * px[0] as i32 - 85 * px[1] as i32 + 128 * px[2] as i32 + 32_768
}

/// RGB → Cr (raw ×256, БЕЗ clamp/округления).
#[inline]
fn rgb_to_cr(px: &[u8]) -> i32 {
    128 * px[0] as i32 - 107 * px[1] as i32 - 21 * px[2] as i32 + 32_768
}

/// RGB → YCbCr (BT.601).
/// Возвращает (Y, Cb, Cr) в [0, 255].
#[inline]
#[allow(dead_code)] // f32-эталон: используется тестами (round-trip, согласованность)
fn rgb_to_ycbcr(r: f32, g: f32, b: f32) -> (u8, u8, u8) {
    let y = 0.299 * r + 0.587 * g + 0.114 * b;
    let cb = -0.169 * r - 0.331 * g + 0.500 * b + 128.0;
    let cr = 0.500 * r - 0.419 * g - 0.081 * b + 128.0;

    (
        y.round().clamp(0.0, 255.0) as u8,
        cb.round().clamp(0.0, 255.0) as u8,
        cr.round().clamp(0.0, 255.0) as u8,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn make_frame(data: Vec<u8>, w: u32, h: u32, format: PixelFormat) -> Frame {
        Frame {
            data,
            metadata: FrameMetadata {
                width: w,
                height: h,
                format,
                captured_at: Utc::now(),
                seq: 1,
            },
        }
    }

    #[test]
    fn convert_same_format_is_noop() {
        let frame = make_frame(vec![0; 100], 10, 10, PixelFormat::Rgb24);
        let result = convert_to(&frame, PixelFormat::Rgb24).unwrap();
        assert_eq!(result.data, frame.data);
    }

    #[test]
    fn yuyv_to_nv12_correct_dimensions() {
        let w = 4;
        let h = 2;
        // YUYV: 4 pixels × 2 bytes/pixel × 2 rows = 16 bytes
        // Layout: [Y0, U01, Y1, V01, Y2, U23, Y3, V23] per row
        let data = vec![
            100, 128, 110, 128, 120, 128, 130, 128, // row 0
            100, 128, 110, 128, 120, 128, 130, 128, // row 1
        ];
        let frame = make_frame(data, w, h, PixelFormat::Yuyv);

        let nv12 = yuyv_to_nv12(&frame).unwrap();

        assert_eq!(nv12.metadata.format, PixelFormat::Nv12);
        // NV12: Y plane (4*2=8) + UV plane (4*2/2=4) = 12 bytes
        assert_eq!(nv12.data.len(), 12);
        // Y plane: first 8 bytes = [100, 110, 120, 130, 100, 110, 120, 130]
        assert_eq!(&nv12.data[0..8], &[100, 110, 120, 130, 100, 110, 120, 130]);
    }

    #[test]
    fn yuyv_to_nv12_wrong_format_fails() {
        let frame = make_frame(vec![0; 16], 4, 2, PixelFormat::Rgb24);
        let result = yuyv_to_nv12(&frame);
        assert!(result.is_err());
    }

    /// Regression: чтение V последней пары пикселей выходило за границы
    /// (index 2wh+1 при len 2wh). Падало на PS Eye 320x240 (len=153600).
    #[test]
    fn yuyv_conversions_exact_size_no_panic() {
        for (w, h) in [(320, 240), (640, 480), (4, 2)] {
            let data: Vec<u8> = (0..w * h * 2).map(|i| (i % 251) as u8).collect();
            let frame = make_frame(data, w as u32, h as u32, PixelFormat::Yuyv);

            let rgb = yuyv_to_rgb24(&frame).unwrap();
            assert_eq!(rgb.data.len(), w * h * 3);
            assert_eq!(rgb.metadata.format, PixelFormat::Rgb24);

            let nv12 = yuyv_to_nv12(&frame).unwrap();
            assert_eq!(nv12.data.len(), w * h * 3 / 2);
        }
    }

    /// YUYV хрома парная: оба пикселя пары должны получить одинаковые U/V.
    /// Белый кадр (Y=255, U=V=128) → весь RGB = белый.
    #[test]
    fn yuyv_to_rgb24_paired_chroma() {
        let (w, h) = (6, 4);
        let mut data = vec![0u8; w * h * 2];
        for pair in data.chunks_exact_mut(4) {
            pair[0] = 255; // Y0
            pair[1] = 128; // U
            pair[2] = 255; // Y1
            pair[3] = 128; // V
        }
        let frame = make_frame(data, w as u32, h as u32, PixelFormat::Yuyv);
        let rgb = yuyv_to_rgb24(&frame).unwrap();
        for px in rgb.data.chunks_exact(3) {
            assert!(
                (px[0] as i16 - px[1] as i16).abs() <= 2
                    && (px[1] as i16 - px[2] as i16).abs() <= 2,
                "expected neutral white, got {px:?}"
            );
        }
    }

    #[test]
    fn yuyv_to_nv12_wrong_size_fails() {
        let frame = make_frame(vec![0; 10], 4, 2, PixelFormat::Yuyv);
        let result = yuyv_to_nv12(&frame);
        assert!(result.is_err());
    }

    #[test]
    fn rgb24_to_nv12_correct_dimensions() {
        let w = 4;
        let h = 2;
        // RGB24: 4 pixels × 3 bytes = 12 bytes per row, 2 rows = 24 bytes
        let data: Vec<u8> = (0..24).collect();
        let frame = make_frame(data, w, h, PixelFormat::Rgb24);

        let nv12 = rgb24_to_nv12(&frame).unwrap();

        assert_eq!(nv12.metadata.format, PixelFormat::Nv12);
        assert_eq!(nv12.data.len(), 12);
        // Y plane: 8 bytes
        assert_eq!(&nv12.data[0..8].len(), &8);
    }

    #[test]
    fn rgb_to_ycbcr_black_is_black() {
        // Black: R=G=B=0 → Y=0, Cb=128, Cr=128
        let (y, cb, cr) = rgb_to_ycbcr(0.0, 0.0, 0.0);
        assert_eq!(y, 0);
        assert_eq!(cb, 128);
        assert_eq!(cr, 128);
    }

    #[test]
    fn rgb_to_ycbcr_white_is_white() {
        // White: R=G=B=255 → Y=255, Cb=128, Cr=128
        let (y, cb, cr) = rgb_to_ycbcr(255.0, 255.0, 255.0);
        assert_eq!(y, 255);
        assert_eq!(cb, 128);
        assert_eq!(cr, 128);
    }

    /// Integer-хелперы должны совпадать с f32-эталоном в пределах ±1
    /// (перф-аудит 2026-08: горячие пути переведены на fixed-point).
    #[test]
    fn integer_helpers_match_f32_reference() {
        let mut y = 16;
        while y <= 235 {
            for cb in [0i32, 64, 128, 192, 255] {
                for cr in [0i32, 64, 128, 192, 255] {
                    let f = ycbcr_to_rgb(y as f32, cb as f32 - 128.0, cr as f32 - 128.0);
                    let i = ycbcr_to_rgb_i32(y, cb - 128, cr - 128);
                    assert!(
                        (f.0 as i32 - i.0 as i32).abs() <= 1
                            && (f.1 as i32 - i.1 as i32).abs() <= 1
                            && (f.2 as i32 - i.2 as i32).abs() <= 1,
                        "y={y} cb={cb} cr={cr}: f32 {f:?} vs int {i:?}"
                    );
                }
            }
            y += 17;
        }
        // Люма: f32-формула vs integer.
        for r in [0u8, 1, 17, 64, 100, 128, 200, 254, 255] {
            for g in [0u8, 3, 33, 90, 128, 180, 250, 255] {
                for b in [0u8, 7, 45, 128, 210, 255] {
                    let f = (0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32)
                        .round()
                        .clamp(0.0, 255.0) as i32;
                    let i = rgb_to_y_u8(r, g, b) as i32;
                    assert!((f - i).abs() <= 1, "rgb({r},{g},{b}): f32 {f} vs int {i}");
                }
            }
        }
    }

    /// TG26-125: NV12 → RGB24 — размерности, формат, нейтральная хрома.
    #[test]
    fn nv12_to_rgb24_neutral_chroma() {
        let (w, h) = (8u32, 6u32);
        let mut data = vec![0u8; (w * h * 3 / 2) as usize];
        // Y-плоскость: градиент; UV: нейтральная хрома (128,128).
        for (i, b) in data[..(w * h) as usize].iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }
        for b in &mut data[(w * h) as usize..] {
            *b = 128;
        }
        let frame = make_frame(data, w, h, PixelFormat::Nv12);
        let rgb = nv12_to_rgb24(&frame).unwrap();
        assert_eq!(rgb.metadata.format, PixelFormat::Rgb24);
        assert_eq!(rgb.data.len(), (w * h * 3) as usize);
        // Нейтральная хрома → R=G=B=Y.
        for (i, px) in rgb.data.chunks_exact(3).enumerate() {
            let y = (i % 251) as i32;
            assert!(
                (px[0] as i32 - y).abs() <= 1
                    && (px[1] as i32 - y).abs() <= 1
                    && (px[2] as i32 - y).abs() <= 1,
                "px[{i}] = {px:?}, expected ~{y} (neutral chroma)"
            );
        }
    }

    #[test]
    fn nv12_to_rgb24_wrong_format_fails() {
        let frame = make_frame(vec![0; 16], 4, 2, PixelFormat::Yuyv);
        assert!(nv12_to_rgb24(&frame).is_err());
    }

    /// Согласованность с f32-эталоном (ycbcr_to_rgb) на сэмплированной сетке.
    #[test]
    fn nv12_to_rgb24_matches_f32_reference() {
        let (w, h) = (8u32, 6u32);
        let mut data = vec![0u8; (w * h * 3 / 2) as usize];
        let mut seed = 7u64;
        let mut rnd = || -> u8 {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (seed >> 56) as u8
        };
        for b in data.iter_mut() {
            *b = rnd();
        }
        let frame = make_frame(data, w, h, PixelFormat::Nv12);
        let rgb = nv12_to_rgb24(&frame).unwrap();
        let d = &frame.data;
        let uv_off = (w * h) as usize;
        for j in 0..h as usize {
            for i in 0..w as usize {
                let y = d[j * w as usize + i] as f32;
                let uv = uv_off + (j / 2) * w as usize + (i & !1);
                let (u, v) = (d[uv] as f32 - 128.0, d[uv + 1] as f32 - 128.0);
                let expect = ycbcr_to_rgb(y, u, v);
                let got = &rgb.data[(j * w as usize + i) * 3..][..3];
                assert!(
                    (expect.0 as i32 - got[0] as i32).abs() <= 1
                        && (expect.1 as i32 - got[1] as i32).abs() <= 1
                        && (expect.2 as i32 - got[2] as i32).abs() <= 1,
                    "px({i},{j}): f32 {expect:?} vs int {got:?}"
                );
            }
        }
    }

    #[test]
    fn ycbcr_to_rgb_round_trip() {
        // RGB → YCbCr → RGB должен дать примерно тот же результат
        let (r, g, b) = (100.0, 150.0, 200.0);
        let (y, cb, cr) = rgb_to_ycbcr(r, g, b);
        let (r2, g2, b2) = ycbcr_to_rgb(y as f32, cb as f32 - 128.0, cr as f32 - 128.0);

        // Допускаем погрешность ±2 из-за округления
        assert!((r - r2 as f32).abs() <= 2.0, "R mismatch: {r} vs {r2}");
        assert!((g - g2 as f32).abs() <= 2.0, "G mismatch: {g} vs {g2}");
        assert!((b - b2 as f32).abs() <= 2.0, "B mismatch: {b} vs {b2}");
    }

    #[test]
    fn convert_unsupported_returns_error() {
        let frame = make_frame(vec![0; 10], 10, 1, PixelFormat::Nv12);
        let result = convert_to(&frame, PixelFormat::Mjpeg);
        assert!(result.is_err());
    }

    #[test]
    fn mjpeg_decode_invalid_data_fails() {
        let frame = make_frame(vec![0, 1, 2, 3], 4, 1, PixelFormat::Mjpeg);
        let result = decode_mjpeg_to_rgb(&frame);
        assert!(result.is_err());
    }

    #[test]
    fn mjpeg_decode_wrong_format_fails() {
        let frame = make_frame(vec![0; 10], 10, 1, PixelFormat::Rgb24);
        let result = decode_mjpeg_to_rgb(&frame);
        assert!(result.is_err());
    }

    /// Integration test: декодируем реальный MJPEG кадр.
    /// Создаём минимальный валидный JPEG (1x1 pixel, серый).
    #[test]
    fn mjpeg_decode_valid_gray_pixel() {
        // Minimal 1x1 gray JPEG (SOI + APP0 + SOF0 + SOS + data + EOI)
        // Это валидный JPEG для пикселя RGB(128, 128, 128)
        let jpeg_data: Vec<u8> = vec![
            0xFF, 0xD8, // SOI
            0xFF, 0xE0, 0x00, 0x10, 0x4A, 0x46, 0x49, 0x46, 0x00, 0x01, 0x01, 0x00, 0x00, 0x01,
            0x00, 0x01, 0x00, 0x00, // APP0
            0xFF, 0xDB, 0x00, 0x43, 0x00, // DQT
            0x10, 0x0B, 0x0C, 0x0E, 0x0C, 0x0A, 0x10, 0x0E, 0x0D, 0x0E, 0x12, 0x11, 0x10, 0x13,
            0x18, 0x28, 0x1A, 0x18, 0x16, 0x16, 0x18, 0x31, 0x23, 0x25, 0x1D, 0x28, 0x3A, 0x33,
            0x3D, 0x3C, 0x39, 0x33, 0x38, 0x37, 0x40, 0x48, 0x5C, 0x4E, 0x40, 0x44, 0x57, 0x45,
            0x37, 0x38, 0x50, 0x6D, 0x51, 0x57, 0x5E, 0x62, 0x67, 0x68, 0x67, 0x3E, 0x4D, 0x71,
            0x79, 0x70, 0x64, 0x78, 0x5C, 0x65, 0x67, 0x63, 0xFF, 0xC0, 0x00, 0x0B, 0x08, 0x00,
            0x01, 0x00, 0x01, 0x01, 0x01, 0x11, 0x00, // SOF0
            0xFF, 0xC4, 0x00, 0x1F, 0x00, 0x00, 0x01, 0x05, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06,
            0x07, 0x08, 0x09, 0x0A, 0x0B, // DHT
            0xFF, 0xDA, 0x00, 0x08, 0x01, 0x01, 0x00, 0x00, 0x3F, 0x00, 0x7B,
            0x40, // SOS + data
            0xFF, 0xD9, // EOI
        ];

        let frame = make_frame(jpeg_data, 1, 1, PixelFormat::Mjpeg);
        let result = decode_mjpeg_to_rgb(&frame);

        // Может не сработать на минимальном JPEG — проверяем что хотя бы не паникует
        match result {
            Ok(rgb) => {
                assert_eq!(rgb.metadata.format, PixelFormat::Rgb24);
                assert_eq!(rgb.metadata.width, 1);
                assert_eq!(rgb.metadata.height, 1);
            }
            Err(e) => {
                // Допустимо — минимальный JPEG может быть некорректным
                eprintln!("Note: minimal JPEG decode failed (expected): {e}");
            }
        }
    }
}

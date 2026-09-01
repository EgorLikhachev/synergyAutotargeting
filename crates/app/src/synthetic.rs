//! Синтетический источник: тёмный фон + яркий квадрат, движущийся по Лиссажу.
//! Для полной локальной отладки гибрида без камеры/NPU.

use common::BBox;

pub fn target_position(w: u32, h: u32, t: f32) -> (f32, f32) {
    let cx = w as f32 * 0.5 + (w as f32 * 0.35) * (0.35 * t).sin();
    let cy = h as f32 * 0.5 + (h as f32 * 0.3) * (0.5 * t + 1.0).cos();
    (cx, cy)
}

pub fn target_bbox(w: u32, h: u32, t: f32) -> BBox {
    let (cx, cy) = target_position(w, h, t);
    // Размер «цели» слегка дышит.
    let s = 56.0 + 8.0 * (0.7 * t).sin();
    BBox::new(cx - s * 0.5, cy - s * 0.5, s, s)
}

pub fn synth_frame(w: u32, h: u32, t: f32) -> common::Frame {
    let mut data = vec![24u8; (w * h * 3) as usize];
    // Лёгкий градиент фона, чтобы у трекера была текстура для ошибок.
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 3) as usize;
            data[i] = (24 + (x * 8 / w)) as u8;
            data[i + 1] = (20 + (y * 6 / h)) as u8;
        }
    }
    // Цель — белый квадрат с чёрной рамкой.
    let bbox = target_bbox(w, h, t);
    let (bx, by, bw, bh) = (
        bbox.x as i32,
        bbox.y as i32,
        bbox.w as i32,
        bbox.h as i32,
    );
    for y in by.max(0)..(by + bh).min(h as i32) {
        for x in bx.max(0)..(bx + bw).min(w as i32) {
            let i = ((y as u32 * w + x as u32) * 3) as usize;
            let edge = x - bx < 4 || y - by < 4 || bx + bw - x < 4 || by + bh - y < 4;
            let v = if edge { 30u8 } else { 235u8 };
            data[i] = v;
            data[i + 1] = v;
            data[i + 2] = v;
        }
    }
    common::Frame::new(data, w, h, common::PixelFormat::Rgb24, t as u64)
}

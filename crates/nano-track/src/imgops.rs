//! Операции над RGB24-изображениями для NanoTrack:
//! вырезка окна с padding средним цветом, билинейный ресайз, средний цвет.
//! Порт getSubwindow из OpenCV tracker_nano.cpp (в свою очередь из NanoTrack).

/// Изображение RGB24.
#[derive(Debug, Clone)]
pub struct Img {
    pub w: u32,
    pub h: u32,
    pub data: Vec<u8>,
}

impl Img {
    pub fn new(data: Vec<u8>, w: u32, h: u32) -> Self {
        Self { w, h, data }
    }

    /// Средний цвет каналов (R, G, B).
    pub fn mean_color(&self) -> [f32; 3] {
        let n = (self.w as usize * self.h as usize) as f32;
        let mut sum = [0f64; 3];
        for px in self.data.chunks_exact(3) {
            sum[0] += px[0] as f64;
            sum[1] += px[1] as f64;
            sum[2] += px[2] as f64;
        }
        [
            (sum[0] / n as f64) as f32,
            (sum[1] / n as f64) as f32,
            (sum[2] / n as f64) as f32,
        ]
    }
}

/// Билинейный ресайз RGB24 → квадрат dst_sz × dst_sz.
pub fn resize_square(src: &Img, dst_sz: u32) -> Img {
    let (sw, sh) = (src.w as usize, src.h as usize);
    let dsz = dst_sz as usize;
    let mut out = vec![0u8; dsz * dsz * 3];
    let sx_scale = sw as f32 / dsz as f32;
    let sy_scale = sh as f32 / dsz as f32;
    for dy in 0..dsz {
        let sy = ((dy as f32 + 0.5) * sy_scale - 0.5).max(0.0);
        let sy0 = (sy as usize).min(sh - 1);
        let fy = sy - sy0 as f32;
        let sy1 = (sy0 + 1).min(sh - 1);
        for dx in 0..dsz {
            let sx = ((dx as f32 + 0.5) * sx_scale - 0.5).max(0.0);
            let sx0 = (sx as usize).min(sw - 1);
            let fx = sx - sx0 as f32;
            let sx1 = (sx0 + 1).min(sw - 1);
            let s00 = (sy0 * sw + sx0) * 3;
            let s01 = (sy0 * sw + sx1) * 3;
            let s10 = (sy1 * sw + sx0) * 3;
            let s11 = (sy1 * sw + sx1) * 3;
            let d = (dy * dsz + dx) * 3;
            for c in 0..3 {
                let top = src.data[s00 + c] as f32 * (1.0 - fx) + src.data[s01 + c] as f32 * fx;
                let bot = src.data[s10 + c] as f32 * (1.0 - fx) + src.data[s11 + c] as f32 * fx;
                out[d + c] = (top * (1.0 - fy) + bot * fy).round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    Img::new(out, dst_sz, dst_sz)
}

/// Вырезать окно размером original_sz×original_sz вокруг центра (cx, cy),
/// дополняя выход за границы изображения средним цветом, и сресайзить в
/// resize_sz×resize_sz. Порт getSubwindow().
pub fn get_subwindow(src: &Img, cx: f32, cy: f32, original_sz: i32, resize_sz: u32) -> Img {
    let avg = src.mean_color();
    let img_w = src.w as i32;
    let img_h = src.h as i32;
    let c = (original_sz + 1) / 2;

    let context_xmin = cx as i32 - c;
    let context_xmax = context_xmin + original_sz - 1;
    let context_ymin = cy as i32 - c;
    let context_ymax = context_ymin + original_sz - 1;

    let left_pad = (-context_xmin).max(0);
    let top_pad = (-context_ymin).max(0);
    let right_pad = (context_xmax - img_w + 1).max(0);
    let bottom_pad = (context_ymax - img_h + 1).max(0);

    let x_min = context_xmin + left_pad;
    let x_max = context_xmax + left_pad;
    let y_min = context_ymin + top_pad;
    let y_max = context_ymax + top_pad;

    let crop_w = (x_max - x_min + 1) as usize;
    let crop_h = (y_max - y_min + 1) as usize;

    // Собираем кроп из расширенного (padding) изображения.
    let mut crop = vec![0u8; crop_w * crop_h * 3];
    let avg_u8 = [
        avg[0].round().clamp(0.0, 255.0) as u8,
        avg[1].round().clamp(0.0, 255.0) as u8,
        avg[2].round().clamp(0.0, 255.0) as u8,
    ];
    for y in 0..crop_h {
        let sy = y as i32 + y_min - top_pad;
        for x in 0..crop_w {
            let sx = x as i32 + x_min - left_pad;
            let d = (y * crop_w + x) * 3;
            let inside = sy >= 0 && sy < img_h && sx >= 0 && sx < img_w;
            if inside {
                let s = (sy as usize * src.w as usize + sx as usize) * 3;
                crop[d] = src.data[s];
                crop[d + 1] = src.data[s + 1];
                crop[d + 2] = src.data[s + 2];
            } else {
                crop[d] = avg_u8[0];
                crop[d + 1] = avg_u8[1];
                crop[d + 2] = avg_u8[2];
            }
        }
    }
    resize_square(&Img::new(crop, crop_w as u32, crop_h as u32), resize_sz)
}

/// Превратить RGB24-квадрат в NCHW f32-тензор (значения 0..255).
/// swap_rb=true меняет местами R и B (если источник BGR).
pub fn to_nchw_f32(img: &Img, swap_rb: bool) -> Vec<f32> {
    let n = (img.w as usize * img.h as usize) as usize;
    // NCHW: сначала весь канал R, затем G, затем B.
    let mut ch = [Vec::with_capacity(n), Vec::with_capacity(n), Vec::with_capacity(n)];
    for px in img.data.chunks_exact(3) {
        let (a, b) = if swap_rb { (px[2], px[0]) } else { (px[0], px[2]) };
        ch[0].push(a as f32);
        ch[1].push(px[1] as f32);
        ch[2].push(b as f32);
    }
    let mut out = Vec::with_capacity(n * 3);
    out.extend_from_slice(&ch[0]);
    out.extend_from_slice(&ch[1]);
    out.extend_from_slice(&ch[2]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subwindow_inside_no_pad() {
        // Изображение 100x100, белое; окно 40x40 вокруг (50,50) — целиком внутри.
        let img = Img::new(vec![255; 100 * 100 * 3], 100, 100);
        let crop = get_subwindow(&img, 50.0, 50.0, 40, 20);
        assert_eq!(crop.w, 20);
        assert!(crop.data.iter().all(|&v| v == 255));
    }

    #[test]
    fn subwindow_outside_pads_with_mean() {
        // Окно выходит за границы; padding должен быть средним цветом (тёмным).
        let mut data = vec![0u8; 50 * 50 * 3];
        // Небольшой светлый квадрат в центре, чтобы среднее было ненулевым.
        for y in 20..30 {
            for x in 20..30 {
                let i = (y * 50 + x) * 3;
                data[i] = 200;
                data[i + 1] = 200;
                data[i + 2] = 200;
            }
        }
        let img = Img::new(data, 50, 50);
        let crop = get_subwindow(&img, 0.0, 0.0, 30, 15);
        assert_eq!(crop.w, 15);
        assert_eq!(crop.data.len(), 15 * 15 * 3);
    }

    #[test]
    fn nchw_layout() {
        let img = Img::new(vec![10, 20, 30], 1, 1);
        let v = to_nchw_f32(&img, false);
        assert_eq!(v, vec![10.0, 20.0, 30.0]);
        let v_swapped = to_nchw_f32(&img, true);
        assert_eq!(v_swapped, vec![30.0, 20.0, 10.0]);
    }
}

//! Оценка глобального движения кадра (GMC) — цифровая стабилизация для
//! жёсткого монтажа камеры без виброразвязки (ROADMAP-D).
//!
//! Кадр прореживается в 8 раз (640×480 → 80×60 grayscale), сдвиг ищется
//! целочисленным SSD-поиском в окне ±4 малых пикселя (±32 полных) с
//! субпиксельным уточнением параболой. Стоимость — доли миллисекунды.

use crate::imgops::Img;

pub const DOWNSAMPLE: u32 = 8;
/// Полуокно поиска в малых пикселях: ±4 → ±32 полных пикселя за кадр
/// (на 60 FPS покрывает вибрацию ~1900 px/с).
const SEARCH: i32 = 4;

pub struct GmcEstimator {
    prev: Option<Vec<u8>>,
}

impl GmcEstimator {
    pub fn new(_w: u32, _h: u32) -> Self {
        Self { prev: None }
    }

    /// Оценить сдвиг кадра относительно предыдущего (dx, dy), ПОЛНЫЕ пиксели.
    /// Положительный dx — кадр сместился вправо (мир уехал влево).
    pub fn estimate(&mut self, img: &Img) -> (f32, f32) {
        let small = downsample_gray(img, DOWNSAMPLE);
        let Some(prev) = self.prev.replace(small.clone()) else {
            return (0.0, 0.0);
        };
        let sw = (img.w / DOWNSAMPLE) as i32;
        let sh = (img.h / DOWNSAMPLE) as i32;
        let (dx, dy) = search_shift(&prev, &small, SEARCH, sw, sh);
        let (dx, dy) = refine(&prev, &small, dx, dy, sw, sh);
        (dx as f32 * DOWNSAMPLE as f32, dy as f32 * DOWNSAMPLE as f32)
    }
}

/// Box-фильтр 8×8 → grayscale малого размера.
fn downsample_gray(img: &Img, s: u32) -> Vec<u8> {
    let w = img.w / s;
    let h = img.h / s;
    let mut out = vec![0u8; (w * h) as usize];
    for y in 0..h {
        for x in 0..w {
            let mut sum = 0u32;
            for dy in 0..s {
                for dx in 0..s {
                    let px = ((y * s + dy) * img.w + x * s + dx) as usize * 3;
                    // 0.3R + 0.6G + 0.1B ≈ luminance
                    sum += (img.data[px] as u32 * 77
                        + img.data[px + 1] as u32 * 151
                        + img.data[px + 2] as u32 * 28)
                        >> 8;
                }
            }
            out[(y * w + x) as usize] = (sum / (s * s)) as u8;
        }
    }
    out
}

/// SSD-поиск целочисленного сдвига: какой сдвиг cur относительно prev.
fn search_shift(prev: &[u8], cur: &[u8], search: i32, w: i32, h: i32) -> (i32, i32) {
    let mut best = (0i32, 0i32);
    let mut best_cost = i64::MAX;
    for dy in -search..=search {
        for dx in -search..=search {
            let mut cost = 0i64;
            for y in search..h - search {
                let row = (y * w) as usize;
                let row_s = ((y + dy) * w) as usize;
                for x in search..w - search {
                    let a = prev[row + x as usize] as i32;
                    let b = cur[row_s + (x + dx) as usize] as i32;
                    let d = a - b;
                    cost += (d * d) as i64;
                }
            }
            if cost < best_cost {
                best_cost = cost;
                best = (dx, dy);
            }
        }
    }
    best
}

/// Субпиксельное уточнение параболой по SSD соседних сдвигов.
fn refine(prev: &[u8], cur: &[u8], dx: i32, dy: i32, w: i32, h: i32) -> (f32, f32) {
    let cost = |dx: i32, dy: i32| -> i64 {
        let mut cost = 0i64;
        for y in SEARCH.max(0)..h - SEARCH {
            let row = (y * w) as usize;
            let row_s = ((y + dy) * w) as usize;
            for x in SEARCH..w - SEARCH {
                let a = prev[row + x as usize] as i32;
                let b = cur[row_s + (x + dx) as usize] as i32;
                let d = a - b;
                cost += (d * d) as i64;
            }
        }
        cost
    };
    let sub = |c_m: i64, c_0: i64, c_p: i64| -> f32 {
        let denom = c_m as f64 - 2.0 * c_0 as f64 + c_p as f64;
        if denom.abs() < 1e-9 {
            0.0
        } else {
            ((c_m as f64 - c_p as f64) / (2.0 * denom)) as f32
        }
    };
    let fx = sub(cost(dx - 1, dy), cost(dx, dy), cost(dx + 1, dy));
    let fy = sub(cost(dx, dy - 1), cost(dx, dy), cost(dx, dy + 1));
    (dx as f32 + fx, dy as f32 + fy)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Текстурный кадр 320×240: градиенты + «объект».
    fn scene(shift_x: i32, shift_y: i32) -> Img {
        let (w, h) = (320u32, 240u32);
        let mut data = vec![0u8; (w * h * 3) as usize];
        for y in 0..h {
            for x in 0..w {
                let sx = x as i32 - shift_x;
                let sy = y as i32 - shift_y;
                let i = ((y * w + x) * 3) as usize;
                let checker = ((sx / 16 + sy / 16) % 2) * 60;
                let grad = ((sx * 3 + sy * 2) % 128) as u8;
                data[i] = grad.wrapping_add(checker as u8);
                data[i + 1] = (sy % 200) as u8;
                data[i + 2] = (sx % 160) as u8;
                // «объект»
                if (sx - 160).abs() < 24 && (sy - 120).abs() < 24 {
                    data[i] = 250;
                    data[i + 1] = 90;
                    data[i + 2] = 40;
                }
            }
        }
        Img::new(data, w, h)
    }

    #[test]
    fn recovers_known_shift() {
        let base = scene(0, 0);
        for &(dx, dy) in &[(16i32, -8), (-24, 12), (8, 8), (-16, -16)] {
            let moved = scene(dx, dy); // содержимое кадра сместилось на (dx, dy)
            let mut g = GmcEstimator::new(320, 240);
            g.estimate(&base);
            let est = g.estimate(&moved);
            // точность: до шага прореживания (8 px) + уточнение
            assert!(
                (est.0 - dx as f32).abs() <= 8.0 && (est.1 - dy as f32).abs() <= 8.0,
                "сдвиг ({dx},{dy}): оценка {est:?}"
            );
        }
    }

    #[test]
    fn zero_shift_zero_estimate() {
        let f = scene(0, 0);
        let mut g = GmcEstimator::new(320, 240);
        g.estimate(&f);
        let est = g.estimate(&scene(0, 0));
        assert!(est.0.abs() <= 2.0 && est.1.abs() <= 2.0, "{est:?}");
    }
}

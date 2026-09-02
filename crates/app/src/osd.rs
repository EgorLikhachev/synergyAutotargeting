//! OSD: отрисовка боксов/прицела/цифр прямо в RGB24-буфер.
//! Зависимостей нет — только пиксели. Шрифт 3×5 (цифры и минимум букв).

use common::BBox;
use pipeline::Mode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rgb {
    Green,
    Cyan,
    Red,
    Yellow,
    #[allow(dead_code)]
    White,
}

impl Rgb {
    fn bytes(self) -> [u8; 3] {
        match self {
            Rgb::Green => [0, 255, 0],
            Rgb::Cyan => [0, 255, 255],
            Rgb::Red => [255, 0, 0],
            Rgb::Yellow => [255, 255, 0],
            Rgb::White => [255, 255, 255],
        }
    }
}

pub fn mode_color(mode: Mode) -> Rgb {
    match mode {
        Mode::Tracking => Rgb::Green,
        Mode::DetectAcquire => Rgb::Cyan,
        Mode::Lost => Rgb::Red,
    }
}

pub fn draw_rect(img: &mut [u8], w: u32, h: u32, bbox: &BBox, color: Rgb, thickness: i32) {
    let (x, y, bw, bh) = bbox.as_ints();
    let (x, y, bw, bh) = (x, y, bw.max(1), bh.max(1));
    let c = color.bytes();
    for t in 0..thickness {
        // Горизонтали
        for dx in 0..bw {
            let px1 = x + dx;
            put_pixel(img, w, h, px1, y + t, c);
            put_pixel(img, w, h, px1, y + bh - 1 - t, c);
        }
        // Вертикали
        for dy in 0..bh {
            put_pixel(img, w, h, x + t, y + dy, c);
            put_pixel(img, w, h, x + bw - 1 - t, y + dy, c);
        }
    }
}

pub fn draw_crosshair(img: &mut [u8], w: u32, h: u32, cx: i32, cy: i32, color: Rgb) {
    let c = color.bytes();
    let r: i32 = 12;
    for d in -r..=r {
        if d.abs() > 3 {
            put_pixel(img, w, h, cx + d, cy, c);
            put_pixel(img, w, h, cx, cy + d, c);
        }
    }
}

/// Надпись цифрами/минимумом букв, масштаб scale (1 = 3×5 пикселей на глиф).
pub fn draw_text(img: &mut [u8], w: u32, h: u32, text: &str, x: i32, y: i32, color: Rgb, scale: i32) {
    let mut cx = x;
    for ch in text.chars() {
        if let Some(glyph) = glyph(ch) {
            draw_glyph(img, w, h, &glyph, cx, y, color, scale);
        }
        cx += 3 * scale + scale; // 3 колонки + межбуквенный пробел
    }
}

fn draw_glyph(
    img: &mut [u8],
    w: u32,
    h: u32,
    glyph: &[u8; 5],
    x: i32,
    y: i32,
    color: Rgb,
    scale: i32,
) {
    let c = color.bytes();
    for row in 0..5i32 {
        for col in 0..3i32 {
            let bit = (glyph[row as usize] >> (2 - col)) & 1;
            if bit == 1 {
                for sy in 0..scale {
                    for sx in 0..scale {
                        put_pixel(
                            img,
                            w,
                            h,
                            x + col * scale + sx,
                            y + row * scale + sy,
                            c,
                        );
                    }
                }
            }
        }
    }
}

#[inline]
fn put_pixel(img: &mut [u8], w: u32, h: u32, x: i32, y: i32, c: [u8; 3]) {
    if x < 0 || y < 0 || x >= w as i32 || y >= h as i32 {
        return;
    }
    let i = (y as u32 * w + x as u32) as usize * 3;
    if i + 2 < img.len() {
        img[i] = c[0];
        img[i + 1] = c[1];
        img[i + 2] = c[2];
    }
}

/// 3×5 глифы: биты слева-направо (старший бит — левая колонка).
fn glyph(ch: char) -> Option<[u8; 5]> {
    let g = match ch {
        '0' => [0b111, 0b101, 0b101, 0b101, 0b111],
        '1' => [0b010, 0b110, 0b010, 0b010, 0b111],
        '2' => [0b111, 0b001, 0b111, 0b100, 0b111],
        '3' => [0b111, 0b001, 0b111, 0b001, 0b111],
        '4' => [0b101, 0b101, 0b111, 0b001, 0b001],
        '5' => [0b111, 0b100, 0b111, 0b001, 0b111],
        '6' => [0b111, 0b100, 0b111, 0b101, 0b111],
        '7' => [0b111, 0b001, 0b001, 0b001, 0b001],
        '8' => [0b111, 0b101, 0b111, 0b101, 0b111],
        '9' => [0b111, 0b101, 0b111, 0b001, 0b111],
        'F' => [0b111, 0b100, 0b111, 0b100, 0b100],
        'P' => [0b111, 0b101, 0b111, 0b100, 0b100],
        'S' => [0b111, 0b100, 0b111, 0b001, 0b111],
        'C' => [0b111, 0b100, 0b100, 0b100, 0b111],
        'T' => [0b111, 0b010, 0b010, 0b010, 0b010],
        'K' => [0b101, 0b101, 0b110, 0b101, 0b101],
        'D' => [0b110, 0b101, 0b101, 0b101, 0b110],
        'L' => [0b100, 0b100, 0b100, 0b100, 0b111],
        '.' => [0b000, 0b000, 0b000, 0b000, 0b010],
        ':' => [0b000, 0b010, 0b000, 0b010, 0b000],
        '-' => [0b000, 0b000, 0b111, 0b000, 0b000],
        ' ' => [0b000, 0b000, 0b000, 0b000, 0b000],
        _ => return None,
    };
    Some(g)
}

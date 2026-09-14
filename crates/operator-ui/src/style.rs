//! Дизайн-система пульта: шрифты (кириллица + значки без «тофу»),
//! палитра, масштаб типографики и размеры элементов.
//!
//! Шрифты: PT Sans (отличная кириллица) как основной + DejaVu Sans
//! как fallback — он закрывает все значки интерфейса (● → ✓ ✗ ⚠ ■ ○),
//! которых нет ни в PT Sans, ни в дефолтном Ubuntu-Light egui
//! (прежде они рисовались пустыми квадратами). Hack остаётся первым
//! в Monospace — им выводятся счётчики (цифры не «пляшут»).
//!
//! Жирное начертание: `RichText::strong()` в egui меняет только ЦВЕТ,
//! не начертание — для настоящего жирного текста завёл отдельное
//! семейство [`BOLD_FAMILY`] (кнопки действий, режим-лампа).

use eframe::egui;
use egui::{Color32, FontData, FontDefinitions, FontFamily, TextStyle};

// ===== Шрифты (встраиваются в exe, внешний путь не нужен) =====

const PT_SANS_REG: &[u8] = include_bytes!("../assets/fonts/PTSans-Regular.ttf");
const PT_SANS_BOLD: &[u8] = include_bytes!("../assets/fonts/PTSans-Bold.ttf");
const DEJAVU_SANS: &[u8] = include_bytes!("../assets/fonts/DejaVuSans.ttf");

/// Имя семейства настоящего жирного начертания (и ключ в font_data).
pub const BOLD_FAMILY: &str = "ptsans-bold";

// ===== Палитра (тёмная) =====

/// Фон панелей (верх/низ).
pub const BG_PANEL: Color32 = Color32::from_rgb(24, 26, 30);
/// Фон «колодца» вокруг видео.
pub const BG_WELL: Color32 = Color32::from_rgb(14, 15, 17);
/// Фон чипов/групп на панели.
pub const CHIP_BG: Color32 = Color32::from_rgb(38, 41, 47);
/// Основной текст.
pub const TEXT: Color32 = Color32::from_rgb(228, 230, 233);
/// Второстепенный текст.
pub const TEXT_DIM: Color32 = Color32::from_rgb(150, 155, 162);

// Семантика «норма / внимание / проблема» — единая по всему пульту.
pub const OK: Color32 = Color32::from_rgb(90, 200, 90);
pub const WARN: Color32 = Color32::from_rgb(230, 170, 40);
pub const FAIL: Color32 = Color32::from_rgb(220, 70, 70);

/// Опасные действия (СТОП записи, АРМ ВКЛ).
pub const DANGER: Color32 = Color32::from_rgb(170, 30, 30);
pub const DANGER_ACTIVE: Color32 = Color32::from_rgb(190, 40, 40);
/// Промежуточное подтверждение («ТОЧНО → РАЗРЕШИТЬ»).
pub const CONFIRM: Color32 = Color32::from_rgb(120, 90, 20);
/// Идёт запись, но сигнал пропал (кнопка СТОП записи).
pub const REC_STALE: Color32 = Color32::from_rgb(185, 115, 25);

// ===== Цвета оверлеев видео — ищет tools/ui_visual_test.py. НЕ ИЗМЕНЯТЬ. =====

pub const CYAN: Color32 = Color32::from_rgb(0, 210, 255);
pub const TOL_GREEN: Color32 = Color32::from_rgb(40, 255, 120);
pub const TOL_AMBER: Color32 = Color32::from_rgb(255, 170, 0);
pub const TRACK_GREEN: Color32 = Color32::from_rgb(60, 220, 90);
pub const ACQUIRE_BLUE: Color32 = Color32::from_rgb(80, 200, 255);
pub const LOST_RED: Color32 = Color32::from_rgb(255, 90, 90);
pub const REC_RED: Color32 = Color32::from_rgb(255, 80, 80);
pub const ARM_RED: Color32 = Color32::from_rgb(220, 40, 40);
pub const ARM_RED_DIM: Color32 = Color32::from_rgb(150, 26, 26);
pub const ARM_PLAQUE: Color32 = Color32::from_rgb(150, 25, 25);

// ===== Размеры =====

/// Высота кнопок верхней панели.
pub const BTN_H_TOP: f32 = 32.0;
/// Высота кнопок действий нижней панели (СТОП/АРМ/СНЯТЬ ЗАХВАТ).
pub const BTN_H_ACTION: f32 = 48.0;
/// Минимальная ширина кнопки СТОП наведения.
pub const W_STOP: f32 = 150.0;
/// Минимальная ширина кнопок АРМ (тексты длинные).
pub const W_ARM: f32 = 190.0;
/// Минимальная ширина «× СНЯТЬ ЗАХВАТ».
pub const W_UNLOCK: f32 = 170.0;
/// Высота зарезервированной строки транзиентных сообщений (панель не прыгает).
pub const MSG_ROW_H: f32 = 24.0;
/// Единое скругление элементов.
pub const ROUNDING: f32 = 6.0;

/// Применить шрифты и стиль к контексту (вызывается один раз при старте).
pub fn install(ctx: &egui::Context) {
    ctx.set_fonts(font_definitions());
    ctx.set_style(style());
}

fn font_definitions() -> FontDefinitions {
    let mut fd = FontDefinitions::default();
    fd.font_data.insert(
        "ptsans".into(),
        FontData::from_static(PT_SANS_REG).tweak(egui::FontTweak {
            // PT Sans чуть крупнее Ubuntu при том же кегле — слегка уменьшаем
            scale: 0.94,
            y_offset_factor: 0.0,
            y_offset: 0.0,
            baseline_offset_factor: 0.0,
        }),
    );
    fd.font_data.insert(
        BOLD_FAMILY.into(),
        FontData::from_static(PT_SANS_BOLD).tweak(egui::FontTweak {
            scale: 0.94,
            y_offset_factor: 0.0,
            y_offset: 0.0,
            baseline_offset_factor: 0.0,
        }),
    );
    fd.font_data.insert("dejavu".into(), FontData::from_static(DEJAVU_SANS));

    // Proportional: PT Sans → DejaVu (значки ● → ✓ ✗ ⚠ ■ ○) → дефолтные
    // (Ubuntu-Light, NotoEmoji, emoji-icon-font остаются как последние fallback).
    let prop = fd.families.entry(FontFamily::Proportional).or_default();
    prop.insert(0, "ptsans".to_owned());
    prop.insert(1, "dejavu".to_owned());
    // Monospace (счётчики): Hack первым, DejaVu — fallback для значков.
    let mono = fd.families.entry(FontFamily::Monospace).or_default();
    mono.insert(1, "dejavu".to_owned());
    // Семейство жирного начертания (см. комментарий модуля).
    fd.families.insert(
        FontFamily::Name(BOLD_FAMILY.into()),
        vec![
            BOLD_FAMILY.to_owned(),
            "dejavu".to_owned(),
            "Ubuntu-Light".to_owned(),
            "NotoEmoji-Regular".to_owned(),
            "emoji-icon-font".to_owned(),
        ],
    );
    fd
}

fn style() -> egui::Style {
    let base = egui::Style::default();
    let spacing = egui::Spacing {
        item_spacing: egui::vec2(8.0, 4.0),
        button_padding: egui::vec2(10.0, 6.0),
        interact_size: egui::vec2(40.0, 20.0),
        ..base.spacing
    };
    // Масштаб типографики: 12/14/20 вместо дефолтных 9/12.5/18.
    egui::Style {
        text_styles: [
            (TextStyle::Small, egui::FontId::new(12.0, FontFamily::Proportional)),
            (TextStyle::Body, egui::FontId::new(14.0, FontFamily::Proportional)),
            (TextStyle::Button, egui::FontId::new(14.0, FontFamily::Proportional)),
            (TextStyle::Heading, egui::FontId::new(20.0, FontFamily::Proportional)),
            (TextStyle::Monospace, egui::FontId::new(13.0, FontFamily::Monospace)),
        ]
        .into(),
        spacing,
        visuals: visuals(),
        ..base
    }
}

fn visuals() -> egui::Visuals {
    let mut v = egui::Visuals::dark();
    v.panel_fill = BG_PANEL;
    v.extreme_bg_color = BG_WELL;
    v.faint_bg_color = CHIP_BG;
    v.hyperlink_color = CYAN;
    v.warn_fg_color = WARN;
    v.error_fg_color = FAIL;

    let r = egui::Rounding::same(ROUNDING);
    v.widgets.noninteractive.rounding = r;
    v.widgets.inactive.rounding = r;
    v.widgets.hovered.rounding = r;
    v.widgets.active.rounding = r;
    v.widgets.open.rounding = r;
    v.window_rounding = egui::Rounding::same(8.0);
    v.menu_rounding = egui::Rounding::same(8.0);

    // Кнопки: спокойная плашка, заметный (но не кричащий) ховер/клик.
    v.widgets.inactive.bg_fill = Color32::from_rgb(45, 48, 55);
    v.widgets.inactive.fg_stroke = egui::Stroke::new(1.0_f32, TEXT);
    v.widgets.hovered.bg_fill = Color32::from_rgb(58, 62, 71);
    v.widgets.hovered.fg_stroke = egui::Stroke::new(1.0_f32, Color32::WHITE);
    v.widgets.active.bg_fill = Color32::from_rgb(66, 71, 82);
    v.widgets.active.fg_stroke = egui::Stroke::new(1.0_f32, Color32::WHITE);
    v.selection.bg_fill = Color32::from_rgb(50, 80, 110);
    v
}

/// Жирный текст (настоящее начертание, не только цвет) — кнопки действий,
/// режим-лампа. Размер/цвет навешиваются следом: `bold("СТОП").size(20.0)`.
pub fn bold(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text).family(FontFamily::Name(BOLD_FAMILY.into()))
}

/// Кнопка действия нижней панели: жирное начертание + единая высота.
/// `fill = None` — нейтральная (стандартная) плашка.
pub fn action_btn(
    label: impl Into<String>,
    size: f32,
    fill: Option<Color32>,
    min_w: f32,
) -> egui::Button<'static> {
    let mut b = egui::Button::new(bold(label).size(size)).min_size(egui::vec2(min_w, BTN_H_ACTION));
    if let Some(f) = fill {
        b = b.fill(f);
    }
    b
}

/// Кнопка верхней панели: единая высота.
pub fn top_btn(label: impl Into<String>) -> egui::Button<'static> {
    egui::Button::new(egui::RichText::new(label).size(13.0))
        .min_size(egui::vec2(0.0, BTN_H_TOP))
}

/// Статус-чип: текст в рамке с фоном (FC, «НАВЕДЕНИЕ РАЗРЕШЕНО», …).
pub fn chip(ui: &mut egui::Ui, text: egui::RichText, col: Color32) {
    egui::Frame::default()
        .fill(CHIP_BG)
        .stroke(egui::Stroke::new(1.0_f32, col))
        .rounding(egui::Rounding::same(5.0))
        .inner_margin(egui::Margin::symmetric(8.0, 3.0))
        .show(ui, |ui| ui.colored_label(col, text));
}

/// Моноширинная метка-счётчик (цифры не меняют ширину — ничего не прыгает).
pub fn counter(ui: &mut egui::Ui, col: Color32, text: String) {
    ui.colored_label(
        col,
        egui::RichText::new(text).monospace().size(13.0),
    );
}

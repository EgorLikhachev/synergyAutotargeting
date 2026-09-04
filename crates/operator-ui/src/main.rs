//! Операторское приложение (ADR-016): стрим с борта + захват цели
//! двойным кликом + АРМ/СТОП наведения.
//!
//! Сетевая роль — приёмник: борт сам подключается (исходящий TCP,
//! NPU-quirk ядра). Видео — push MJPEG на :9000, управление — JSON-строки
//! на :9010 (default, меняется флагом --control-port).

mod net;

use eframe::egui;
use egui::{Color32, ColorImage, Pos2, Rect, Sense, Stroke, TextureHandle, Vec2};

use net::{NetState, UiCommand};

const FRAME_W: usize = 640;
const FRAME_H: usize = 480;
/// Сторона ROI ручного захвата (двойной клик), px кадра.
const LOCK_ROI_PX: f32 = 100.0;

fn main() -> eframe::Result<()> {
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([980.0, 760.0])
            .with_title("synergy operator"),
        ..Default::default()
    };
    eframe::run_native("synergy operator", opts, Box::new(|cc| {
        Ok(Box::new(OperatorApp::new(cc)))
    }))
}

struct OperatorApp {
    net: NetState,
    texture: TextureHandle,
    /// Версия кадра в текстуре (чтобы не апдейтить каждые 60 FPS UI).
    tex_version: u64,
    /// Экранная geom видео-панели для трансформа кликов.
    video_rect: Option<Rect>,
    arm_confirm: bool,
    /// Когда включено подтверждение АРМ (автосброс через 4 с).
    arm_confirm_at: Option<std::time::Instant>,
    lock_flash_ms: Option<std::time::Instant>,
    ui_fps: f32,
    ui_fps_acc: (std::time::Instant, u32),
    last_frame_time: std::time::Instant,
}

impl OperatorApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let texture = cc.egui_ctx.load_texture(
            "video",
            ColorImage::new([FRAME_W, FRAME_H], Color32::BLACK),
            Default::default(),
        );
        Self {
            net: NetState::new(),
            texture,
            tex_version: 0,
            video_rect: None,
            arm_confirm: false,
            arm_confirm_at: None,
            lock_flash_ms: None,
            ui_fps: 0.0,
            ui_fps_acc: (std::time::Instant::now(), 0),
            last_frame_time: std::time::Instant::now(),
        }
    }

    /// Экранная точка → координаты кадра (учёт letterbox/масштаба).
    fn screen_to_frame(&self, p: Pos2) -> Option<(f32, f32)> {
        let r = self.video_rect?;
        let scale = (r.width() / FRAME_W as f32).min(r.height() / FRAME_H as f32);
        let vw = FRAME_W as f32 * scale;
        let vh = FRAME_H as f32 * scale;
        let ox = r.left() + (r.width() - vw) / 2.0;
        let oy = r.top() + (r.height() - vh) / 2.0;
        let fx = (p.x - ox) / scale;
        let fy = (p.y - oy) / scale;
        (fx >= 0.0 && fy >= 0.0 && fx < FRAME_W as f32 && fy < FRAME_H as f32)
            .then_some((fx, fy))
    }

    fn frame_to_screen(&self, x: f32, y: f32) -> Option<Pos2> {
        let r = self.video_rect?;
        let scale = (r.width() / FRAME_W as f32).min(r.height() / FRAME_H as f32);
        let vw = FRAME_W as f32 * scale;
        let vh = FRAME_H as f32 * scale;
        let ox = r.left() + (r.width() - vw) / 2.0;
        let oy = r.top() + (r.height() - vh) / 2.0;
        Some(Pos2::new(ox + x * scale, oy + y * scale))
    }

    fn send_lock(&mut self, x: f32, y: f32) {
        self.net.send(UiCommand::Lock { x, y, size: LOCK_ROI_PX });
        self.lock_flash_ms = Some(std::time::Instant::now());
    }
}

impl eframe::App for OperatorApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // FPS UI
        let now = std::time::Instant::now();
        let dt = now.duration_since(self.last_frame_time).as_secs_f32();
        self.last_frame_time = now;
        self.ui_fps_acc.1 += 1;
        if self.ui_fps_acc.0.elapsed() >= std::time::Duration::from_millis(500) {
            self.ui_fps = self.ui_fps_acc.1 as f32
                / self.ui_fps_acc.0.elapsed().as_secs_f32();
            self.ui_fps_acc = (std::time::Instant::now(), 0);
        }

        // подгрузка кадра
        if let Some(frame) = self.net.take_video_frame() {
            if frame.version != self.tex_version {
                self.tex_version = frame.version;
                self.texture.set(
                    ColorImage::from_rgba_unmultiplied([FRAME_W, FRAME_H], &frame.rgba),
                    Default::default(),
                );
            }
        }

        let status = self.net.status();
        let video_ok = self.net.video_connected();
        let ctl_ok = self.net.control_connected();

        // Верхняя панель: связь + счётчики
        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.horizontal(|ui| {
                dot(ui, video_ok, "видео");
                dot(ui, ctl_ok, "управление");
                ui.separator();
                ui.label(format!(
                    "видео {} FPS · UI {:.0} FPS",
                    self.net.video_fps(),
                    self.ui_fps
                ));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("⤢ Полный экран").clicked() {
                        // тумблер fullscreen
                        let fs = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
                        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fs));
                    }
                });
            });
        });

        // Нижняя панель: статус + кнопки
        egui::TopBottomPanel::bottom("bottom").show(ctx, |ui| {
            ui.add_space(6.0);
            // строка статуса
            ui.horizontal(|ui| {
                let (mode_col, mode_txt) = match status.as_ref().map(|s| s.mode.as_str()) {
                    Some("TRACK") => (Color32::from_rgb(60, 220, 90), "TRACK"),
                    Some("ACQUIRE") => (Color32::from_rgb(80, 200, 255), "ACQUIRE"),
                    Some("LOST") => (Color32::from_rgb(255, 90, 90), "LOST"),
                    _ => (Color32::GRAY, "—"),
                };
                ui.colored_label(mode_col, egui::RichText::new(mode_txt).size(22.0).strong());
                if let Some(s) = &status {
                    ui.label(format!(
                        "score {:.2} · FPS {:.0} · e2e {:.1} мс · дет: {} · кадр {}",
                        s.score, s.fps, s.e2e_ms, s.dets.len(), s.frame_seq
                    ));
                } else {
                    ui.weak("нет данных");
                }
            });
            ui.add_space(4.0);
            // кнопки
            ui.horizontal(|ui| {
                // Состояние наведения — крупно и однозначно.
                let armed = status.as_ref().map(|s| s.armed).unwrap_or(false);
                if armed {
                    ui.label(
                        egui::RichText::new("● НАВЕДЕНИЕ РАЗРЕШЕНО")
                            .size(14.0)
                            .strong()
                            .color(Color32::from_rgb(230, 60, 60)),
                    );
                } else {
                    ui.label(
                        egui::RichText::new("○ наведение запрещено")
                            .size(14.0)
                            .color(Color32::from_rgb(120, 160, 120)),
                    );
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // СТОП — большая красная (всегда доступна)
                    let stop_btn = egui::Button::new(
                        egui::RichText::new("СТОП").size(20.0).strong(),
                    )
                    .fill(Color32::from_rgb(150, 30, 30))
                    .min_size(egui::vec2(110.0, 40.0));
                    if ui.add(stop_btn).clicked() {
                        self.net.send(UiCommand::Stop);
                        self.arm_confirm = false;
                        self.arm_confirm_at = None;
                    }
                    // АРМ — двухшаговое подтверждение (безопасность):
                    // первый клик только «заряжает» кнопку, второй включает.
                    if armed {
                        let off = egui::Button::new(
                            egui::RichText::new("● АРМ ВКЛ — выключить").size(18.0).strong(),
                        )
                        .fill(Color32::from_rgb(190, 40, 40))
                        .min_size(egui::vec2(170.0, 40.0));
                        if ui.add(off).clicked() {
                            self.net.send(UiCommand::Arm { on: false });
                        }
                    } else if self.arm_confirm {
                        let yes = egui::Button::new(
                            egui::RichText::new("ТОЧНО → РАЗРЕШИТЬ").size(18.0).strong(),
                        )
                        .fill(Color32::from_rgb(120, 90, 20))
                        .min_size(egui::vec2(170.0, 40.0));
                        if ui.add(yes).clicked() {
                            self.net.send(UiCommand::Arm { on: true });
                            self.arm_confirm = false;
                            self.arm_confirm_at = None;
                        }
                    } else {
                        let arm = egui::Button::new(
                            egui::RichText::new("АРМ (2 клика)").size(18.0).strong(),
                        )
                        .min_size(egui::vec2(170.0, 40.0));
                        if ui.add(arm).clicked() {
                            self.arm_confirm = true;
                            self.arm_confirm_at = Some(std::time::Instant::now());
                        }
                    }
                });
            });
            // Подсказка подтверждения (автосброс 4 с — не даём «заряженной»
            // кнопке висеть бесконечно).
            if self.arm_confirm {
                let left = 4.0 - self
                    .arm_confirm_at
                    .map(|t| t.elapsed().as_secs_f32())
                    .unwrap_or(4.0);
                if left <= 0.0 {
                    self.arm_confirm = false;
                    self.arm_confirm_at = None;
                } else {
                    ui.label(
                        egui::RichText::new(format!(
                            "⚠ нажмите «ТОЧНО → РАЗРЕШИТЬ» в течение {left:.1} с, \
                             иначе подтверждение снимется"
                        ))
                        .size(13.0)
                        .color(Color32::from_rgb(220, 180, 60)),
                    );
                }
            }
            ui.add_space(6.0);
        });

        // Центр: видео
        egui::CentralPanel::default().show(ctx, |ui| {
            let avail = ui.available_size();
            let (rect, resp) = ui.allocate_exact_size(avail, Sense::click());
            self.video_rect = Some(rect);
            // letterbox-подгонка текстуры
            let scale = (rect.width() / FRAME_W as f32).min(rect.height() / FRAME_H as f32);
            let vw = FRAME_W as f32 * scale;
            let vh = FRAME_H as f32 * scale;
            let vrect = Rect::from_min_size(
                Pos2::new(rect.left() + (rect.width() - vw) / 2.0, rect.top() + (rect.height() - vh) / 2.0),
                Vec2::new(vw, vh),
            );
            ui.painter().rect_filled(rect, 0.0, ui.visuals().panel_fill);
            if video_ok {
                ui.painter()
                    .image(self.texture.id(), vrect, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
            } else {
                ui.painter().text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "ждём борт…\nзапустите: synergy --ui <этот-хост>:9010",
                    egui::FontId::proportional(22.0),
                    Color32::GRAY,
                );
            }

            // оверлеи: детекции (красные), цель (зелёная/цвет режима)
            if let Some(s) = &status {
                for d in &s.dets {
                    if let Some(p) = self.frame_to_screen(d.0, d.1) {
                        let wh = Pos2::new(
                            p.x + d.2 * scale,
                            p.y + d.3 * scale,
                        );
                        ui.painter().rect_stroke(
                            Rect::from_two_pos(p, wh),
                            0.0,
                            Stroke::new(1.5, Color32::from_rgb(255, 70, 70)),
                        );
                    }
                }
                if let Some(b) = s.box_xywh {
                    let col = match s.mode.as_str() {
                        "TRACK" => Color32::from_rgb(60, 220, 90),
                        "ACQUIRE" => Color32::from_rgb(80, 200, 255),
                        _ => Color32::from_rgb(255, 90, 90),
                    };
                    if let Some(p) = self.frame_to_screen(b[0] as f32, b[1] as f32) {
                        let wh = Pos2::new(p.x + b[2] as f32 * scale, p.y + b[3] as f32 * scale);
                        ui.painter().rect_stroke(
                            Rect::from_two_pos(p, wh),
                            0.0,
                            Stroke::new(2.5, col),
                        );
                        // перекрестие центра
                        let c = Pos2::new(
                            p.x + b[2] as f32 * scale / 2.0,
                            p.y + b[3] as f32 * scale / 2.0,
                        );
                        ui.painter().line_segment(
                            [Pos2::new(c.x - 10.0, c.y), Pos2::new(c.x + 10.0, c.y)],
                            Stroke::new(1.5, col),
                        );
                        ui.painter().line_segment(
                            [Pos2::new(c.x, c.y - 10.0), Pos2::new(c.x, c.y + 10.0)],
                            Stroke::new(1.5, col),
                        );
                    }
                }
            }

            // вспышка LOCK SENT
            if let Some(t) = self.lock_flash_ms {
                if t.elapsed().as_millis() < 600 {
                    ui.painter().text(
                        vrect.center_top() + Vec2::new(0.0, 30.0),
                        egui::Align2::CENTER_CENTER,
                        "ЗАХВАТ ОТПРАВЛЕН",
                        egui::FontId::proportional(20.0),
                        Color32::from_rgb(120, 220, 255),
                    );
                } else {
                    self.lock_flash_ms = None;
                }
            }

            // перекрестие курсора
            if let Some(hover) = resp.hover_pos() {
                if vrect.contains(hover) {
                    let c = hover;
                    ui.painter().line_segment(
                        [Pos2::new(c.x - 14.0, c.y), Pos2::new(c.x + 14.0, c.y)],
                        Stroke::new(1.0, Color32::from_white_alpha(180)),
                    );
                    ui.painter().line_segment(
                        [Pos2::new(c.x, c.y - 14.0), Pos2::new(c.x, c.y + 14.0)],
                        Stroke::new(1.0, Color32::from_white_alpha(180)),
                    );
                }
            }

            // двойной клик = LOCK
            if resp.double_clicked() {
                if let Some(hover) = resp.interact_pointer_pos() {
                    if let Some((fx, fy)) = self.screen_to_frame(hover) {
                        self.send_lock(fx, fy);
                    }
                }
            }
        });

        // repaint с сетевой частотой (видео ~15-30 Гц)
        ctx.request_repaint_after(std::time::Duration::from_millis(33));
    }
}

fn dot(ui: &mut egui::Ui, ok: bool, label: &str) {
    let col = if ok {
        Color32::from_rgb(60, 220, 90)
    } else {
        Color32::from_rgb(255, 90, 90)
    };
    let (pos, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), Sense::hover());
    ui.painter()
        .circle_filled(pos.center(), 5.0, col);
    ui.label(label).on_hover_text(if ok { "подключено" } else { "нет связи" });
}

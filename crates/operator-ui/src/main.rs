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
            .with_min_inner_size([860.0, 620.0])
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
    /// Фактический размер кадра борта (может отличаться от 640×480).
    frame_wh: (usize, usize),
    /// Экранная geom видео-панели для трансформа кликов.
    video_rect: Option<Rect>,
    arm_confirm: bool,
    /// Когда включено подтверждение АРМ (автосброс через 4 с).
    arm_confirm_at: Option<std::time::Instant>,
    lock_flash_ms: Option<std::time::Instant>,
    ui_fps: f32,
    ui_fps_acc: (std::time::Instant, u32),
    last_frame_time: std::time::Instant,
    /// Путь последней сохранённой записи (показываем ~10 c после стопа).
    last_rec: Option<(String, std::time::Instant)>,
    /// Ошибка запуска записи (показываем ~5 c).
    rec_error: Option<(String, std::time::Instant)>,
    /// «Нет связи» после клика СТОП (показываем ~3 с).
    no_link_flash: Option<std::time::Instant>,
    /// Последний установленный заголовок окна (не слать команду зря).
    last_title: String,
}

impl OperatorApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let texture = cc.egui_ctx.load_texture(
            "video",
            ColorImage::new([FRAME_W, FRAME_H], Color32::BLACK),
            Default::default(),
        );
        Self {
            net: NetState::new(Some(cc.egui_ctx.clone())),
            texture,
            tex_version: 0,
            frame_wh: (FRAME_W, FRAME_H),
            video_rect: None,
            arm_confirm: false,
            arm_confirm_at: None,
            lock_flash_ms: None,
            ui_fps: 0.0,
            ui_fps_acc: (std::time::Instant::now(), 0),
            last_frame_time: std::time::Instant::now(),
            last_rec: None,
            rec_error: None,
            no_link_flash: None,
            last_title: String::new(),
        }
    }

    /// Каталог записей: рядом с exe (dist-папка/ярлык держат WorkingDirectory).
    fn records_dir() -> std::path::PathBuf {
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("records")))
            .unwrap_or_else(|| std::path::PathBuf::from("records"))
    }

    /// Экранная точка → координаты кадра (учёт letterbox/масштаба).
    fn screen_to_frame(&self, p: Pos2) -> Option<(f32, f32)> {
        let r = self.video_rect?;
        let (fw, fh) = (self.frame_wh.0 as f32, self.frame_wh.1 as f32);
        let scale = (r.width() / fw).min(r.height() / fh);
        let vw = fw * scale;
        let vh = fh * scale;
        let ox = r.left() + (r.width() - vw) / 2.0;
        let oy = r.top() + (r.height() - vh) / 2.0;
        let fx = (p.x - ox) / scale;
        let fy = (p.y - oy) / scale;
        (fx >= 0.0 && fy >= 0.0 && fx < fw && fy < fh).then_some((fx, fy))
    }

    fn frame_to_screen(&self, x: f32, y: f32) -> Option<Pos2> {
        let r = self.video_rect?;
        let (fw, fh) = (self.frame_wh.0 as f32, self.frame_wh.1 as f32);
        let scale = (r.width() / fw).min(r.height() / fh);
        let vw = fw * scale;
        let vh = fh * scale;
        let ox = r.left() + (r.width() - vw) / 2.0;
        let oy = r.top() + (r.height() - vh) / 2.0;
        Some(Pos2::new(ox + x * scale, oy + y * scale))
    }

    /// (scale, rect видео в экране) для letterbox-отрисовки.
    fn video_geom(&self, r: Rect) -> (f32, Rect) {
        let (fw, fh) = (self.frame_wh.0 as f32, self.frame_wh.1 as f32);
        let scale = (r.width() / fw).min(r.height() / fh);
        let vw = fw * scale;
        let vh = fh * scale;
        (
            scale,
            Rect::from_min_size(
                Pos2::new(r.left() + (r.width() - vw) / 2.0, r.top() + (r.height() - vh) / 2.0),
                Vec2::new(vw, vh),
            ),
        )
    }

    /// Снять/поставить запись (кнопка или клавиша R).
    fn toggle_recording(&mut self) {
        let rec = self.net.recorder();
        if rec.is_recording() {
            if let Some(p) = rec.stop() {
                self.last_rec = Some((p.display().to_string(), std::time::Instant::now()));
            }
        } else {
            match rec.start(&Self::records_dir()) {
                Ok(_) => self.rec_error = None,
                Err(e) => self.rec_error = Some((e, std::time::Instant::now())),
            }
        }
    }

    /// Полный экран (кнопка или клавиша F).
    fn toggle_fullscreen(ctx: &egui::Context) {
        let fs = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fs));
    }

    fn send_stop(&mut self, ctl_ok: bool) {
        if ctl_ok {
            self.net.send(UiCommand::Stop);
        } else {
            // Канал мёртв: команда не уйдёт — говорим оператору, а не молчим.
            self.no_link_flash = Some(std::time::Instant::now());
        }
        self.arm_confirm = false;
        self.arm_confirm_at = None;
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
        let _dt = now.duration_since(self.last_frame_time).as_secs_f32();
        self.last_frame_time = now;
        self.ui_fps_acc.1 += 1;
        if self.ui_fps_acc.0.elapsed() >= std::time::Duration::from_millis(500) {
            self.ui_fps = self.ui_fps_acc.1 as f32
                / self.ui_fps_acc.0.elapsed().as_secs_f32();
            self.ui_fps_acc = (std::time::Instant::now(), 0);
        }

        // подгрузка кадра (размер берём фактический — борт может прислать
        // не 640×480, паниковать на этом нельзя)
        if let Some(mut frame) = self.net.take_video_frame() {
            if frame.version != self.tex_version {
                self.tex_version = frame.version;
                if (frame.w, frame.h) != self.frame_wh {
                    self.frame_wh = (frame.w, frame.h);
                }
                // ColorImage строится ВЛАДЕНИЕМ пикселей — без копии
                // (from_rgba_unminiplied копировал 1.2 МБ на кадр).
                let image = ColorImage {
                    size: [frame.w, frame.h],
                    pixels: std::mem::take(&mut frame.rgba),
                };
                self.texture.set(image, Default::default());
            }
        }

        let status = self.net.status();
        let status_age = self.net.status_age_secs();
        let video_ok = self.net.video_connected();
        let ctl_ok = self.net.control_connected();

        // Горячие клавиши оператора: Esc — СТОП наведения, F — полный
        // экран, R — запись. Работают в любом месте окна (полей ввода нет).
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.send_stop(ctl_ok);
        }
        if ctx.input(|i| i.key_pressed(egui::Key::F)) {
            Self::toggle_fullscreen(ctx);
        }
        if ctx.input(|i| i.key_pressed(egui::Key::R)) && video_ok {
            self.toggle_recording();
        }

        // Автостоп записи при ошибке диска (писатель умер).
        {
            let rec = self.net.recorder();
            if rec.is_recording() {
                if let Some(e) = rec.stats().error.clone() {
                    rec.stop();
                    self.rec_error = Some((e, std::time::Instant::now()));
                }
            }
        }

        // Живой заголовок окна: состояние видно из панели задач.
        {
            let rec = self.net.recorder();
            let mut t = String::from("synergy");
            if !video_ok && !ctl_ok {
                t.push_str(" · нет связи");
            } else if !ctl_ok {
                t.push_str(" · нет управления");
            } else if !video_ok {
                t.push_str(" · нет видео");
            }
            if rec.is_recording() {
                t.push_str(" · ЗАПИСЬ");
            }
            if status.as_ref().map(|s| s.armed).unwrap_or(false) {
                t.push_str(" · АРМ");
            }
            if t != self.last_title {
                self.last_title = t.clone();
                ctx.send_viewport_cmd(egui::ViewportCommand::Title(t));
            }
        }

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
                    if ui.button("Во весь экран (F)").clicked() {
                        Self::toggle_fullscreen(ctx);
                    }
                    if ui.button("Папка записей").clicked() {
                        open_in_explorer(&Self::records_dir());
                    }
                    ui.separator();
                    // ЗАПИСЬ стрима в .mjpg (replay-формат борта, открывается VLC)
                    let rec = self.net.recorder();
                    if rec.is_recording() {
                        let st = rec.stats();
                        let secs = st.started.map(|t| t.elapsed().as_secs()).unwrap_or(0);
                        let (mm, ss) = (secs / 60, secs % 60);
                        let mut label = format!(
                            "■ СТОП · {:02}:{:02} · {} кадров · {:.1} МБ",
                            mm, ss, st.frames, st.bytes as f32 / 1e6
                        );
                        if st.dropped > 0 {
                            label.push_str(&format!(" · сброшено {}", st.dropped));
                        }
                        if !video_ok {
                            label.push_str(" · НЕТ СИГНАЛА");
                        }
                        let btn = egui::Button::new(
                            egui::RichText::new(label).size(14.0).strong(),
                        )
                        .fill(Color32::from_rgb(170, 30, 30))
                        .min_size(egui::vec2(210.0, 30.0));
                        if ui.add(btn).clicked() {
                            self.toggle_recording();
                        }
                    } else {
                        let btn = egui::Button::new(
                            egui::RichText::new("● ЗАПИСЬ (R)").size(14.0).strong(),
                        )
                        .min_size(egui::vec2(130.0, 30.0));
                        let resp = ui.add_enabled(video_ok, btn);
                        if resp.clicked() {
                            self.toggle_recording();
                        }
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
                    Some("IDLE") => (Color32::GRAY, "ОЖИДАНИЕ"),
                    _ => (Color32::GRAY, "—"),
                };
                ui.colored_label(mode_col, egui::RichText::new(mode_txt).size(22.0).strong());
                if let Some(s) = &status {
                    ui.label(format!(
                        "score {:.2} · FPS {:.0} · e2e {:.1} мс · дет: {} · кадр {}",
                        s.score, s.fps, s.e2e_ms, s.dets.len(), s.frame_seq
                    ));
                    // Возраст данных: зависший борт виден сразу
                    if let Some(age) = status_age {
                        if age > 2.0 {
                            ui.label(
                                egui::RichText::new(format!("данные {age:.0} с назад"))
                                    .size(14.0)
                                    .color(Color32::from_rgb(220, 180, 60)),
                            );
                        }
                    }
                } else if ctl_ok {
                    ui.weak("нет данных от борта");
                } else {
                    ui.weak("канал управления потерян");
                }
            });
            // Индикатор FC (ADR-021): видит ли полётник наш RC-поток.
            match status.as_ref().and_then(|s| s.fc.as_ref()) {
                Some(fc) if fc.online => {
                    // Строка 1: состояние связи; строка 2: эхо каналов из
                    // MSP_RC (функциональный порядок FC: R,P,T,Y,AUX1=ARM…)
                    // — то, что полётник РЕАЛЬНО принимает. Мкс 1000..2000.
                    if fc.rx_ok {
                        ui.label(
                            egui::RichText::new("FC: СВЯЗЬ ОК, RC ПРИНИМАЕТСЯ")
                                .size(12.0)
                                .color(Color32::from_rgb(90, 190, 90)),
                        );
                    } else {
                        ui.label(
                            egui::RichText::new("FC: СВЯЗЬ ОК, НО RC НЕ ВИДИТ (RXLOSS)")
                                .size(12.0)
                                .strong()
                                .color(Color32::from_rgb(230, 130, 40)),
                        );
                    }
                    ui.horizontal(|ui| {
                        const LABELS: [&str; 8] = ["R", "P", "T", "Y", "ARM", "A2", "A3", "A4"];
                        for (i, v) in fc.ch.iter().take(6).enumerate() {
                            let frac = ((*v as f32 - 1000.0) / 1000.0).clamp(0.0, 1.0);
                            let armed_ch = i == 4 && *v > 1700;
                            let bar = egui::ProgressBar::new(frac)
                                .desired_width(40.0)
                                .text(format!("{} {}", LABELS[i], v))
                                .fill(if armed_ch {
                                    Color32::from_rgb(200, 60, 60)
                                } else {
                                    Color32::from_rgb(90, 150, 200)
                                });
                            ui.add(bar);
                        }
                    });
                    // Почему FC не вооружится: armingDisableFlags BF 4.4.3
                    // (таблица в commander::msp). Не путать с нашим armed:
                    // это блокировки НА СТОРОНЕ ПОЛЁТНИКА.
                    if !status.as_ref().is_some_and(|s| s.armed) {
                        let blockers = commander::msp::arming_disable_names(fc.flags);
                        if !blockers.is_empty() {
                            let head: Vec<&str> =
                                blockers.iter().take(3).copied().collect();
                            let extra = blockers.len() - head.len();
                            let text = if extra > 0 {
                                format!("АРМ-блок FC: {} (+{})", head.join(", "), extra)
                            } else {
                                format!("АРМ-блок FC: {}", head.join(", "))
                            };
                            ui.label(
                                egui::RichText::new(text)
                                    .size(10.0)
                                    .color(Color32::from_rgb(200, 160, 60)),
                            );
                        }
                    }
                }
                Some(_) => {
                    ui.label(
                        egui::RichText::new("FC: НЕ ОТВЕЧАЕТ (нет телеметрии)")
                            .size(12.0)
                            .color(Color32::from_rgb(200, 80, 80)),
                    );
                }
                None => {}
            }
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
                        egui::RichText::new("СТОП (Esc)").size(20.0).strong(),
                    )
                    .fill(Color32::from_rgb(150, 30, 30))
                    .min_size(egui::vec2(110.0, 40.0));
                    if ui.add(stop_btn).clicked() {
                        self.send_stop(ctl_ok);
                    }
                    // СНЯТЬ ЗАХВАТ: сбросить цель и трекинг; авто-захват
                    // выключен до следующего двойного клика (режим ОЖИДАНИЕ).
                    // Красные рамки детекций остаются — оператор выбирает
                    // следующую цель. Наведение уходит в центры.
                    let track_engaged = matches!(
                        status.as_ref().map(|s| s.mode.as_str()),
                        Some("TRACK") | Some("ACQUIRE") | Some("LOST")
                    );
                    let unl = egui::Button::new(
                        egui::RichText::new("× СНЯТЬ ЗАХВАТ").size(15.0).strong(),
                    )
                    .min_size(egui::vec2(150.0, 36.0));
                    if ui.add_enabled(track_engaged, unl).clicked() {
                        self.net.send(UiCommand::Unlock);
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
                        .size(14.5)
                        .color(Color32::from_rgb(220, 180, 60)),
                    );
                }
            }
            // путь сохранённой записи: клик — копировать, кнопка — открыть папку
            if let Some((path, t)) = &self.last_rec {
                if t.elapsed().as_secs_f32() < 10.0 {
                    ui.horizontal(|ui| {
                        if ui
                            .button(
                                egui::RichText::new(format!("запись сохранена: {path}"))
                                    .size(14.5)
                                    .color(Color32::from_rgb(120, 200, 120)),
                            )
                            .on_hover_text("клик — скопировать путь")
                            .clicked()
                        {
                            ctx.copy_text(path.clone());
                        }
                        if ui.button("открыть папку").clicked() {
                            open_in_explorer(&Self::records_dir());
                        }
                    });
                } else {
                    self.last_rec = None;
                }
            }
            if let Some((e, t)) = &self.rec_error {
                if t.elapsed().as_secs_f32() < 5.0 {
                    ui.label(
                        egui::RichText::new(format!("ошибка записи: {e}"))
                            .size(14.5)
                            .color(Color32::from_rgb(230, 90, 90)),
                    );
                } else {
                    self.rec_error = None;
                }
            }
            if let Some(t) = &self.no_link_flash {
                if t.elapsed().as_secs_f32() < 3.0 {
                    ui.label(
                        egui::RichText::new("нет связи с бортом — команда не отправлена")
                            .size(14.5)
                            .color(Color32::from_rgb(220, 180, 60)),
                    );
                } else {
                    self.no_link_flash = None;
                }
            }
            ui.add_space(6.0);
        });

        // Центр: видео
        egui::CentralPanel::default().show(ctx, |ui| {
            let avail = ui.available_size();
            let (rect, resp) = ui.allocate_exact_size(avail, Sense::click());
            self.video_rect = Some(rect);
            // letterbox-подгонка текстуры под фактический размер кадра
            let (scale, vrect) = self.video_geom(rect);
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
                            Stroke::new(1.5_f32, Color32::from_rgb(255, 70, 70)),
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
                            Stroke::new(2.5_f32, col),
                        );
                        // перекрестие центра
                        let c = Pos2::new(
                            p.x + b[2] as f32 * scale / 2.0,
                            p.y + b[3] as f32 * scale / 2.0,
                        );
                        ui.painter().line_segment(
                            [Pos2::new(c.x - 10.0, c.y), Pos2::new(c.x + 10.0, c.y)],
                            Stroke::new(1.5_f32, col),
                        );
                        ui.painter().line_segment(
                            [Pos2::new(c.x, c.y - 10.0), Pos2::new(c.x, c.y + 10.0)],
                            Stroke::new(1.5_f32, col),
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
                        Stroke::new(1.0_f32, Color32::from_white_alpha(180)),
                    );
                    ui.painter().line_segment(
                        [Pos2::new(c.x, c.y - 14.0), Pos2::new(c.x, c.y + 14.0)],
                        Stroke::new(1.0_f32, Color32::from_white_alpha(180)),
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

        // repaint со стримом ~30 Гц; без видео — реже (нечего обновлять)
        if video_ok {
            ctx.request_repaint_after(std::time::Duration::from_millis(33));
        } else {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
    }
}

impl Drop for OperatorApp {
    /// Выход из приложения (закрытие окна — eframe дропает app): корректно
    /// завершаем запись (флаш файла с ожиданием писателя), чтобы не
    /// терять хвост буфера.
    fn drop(&mut self) {
        self.net.recorder().stop();
    }
}

/// Открыть папку в Проводнике (кнопки «Папка записей»/«открыть папку»).
fn open_in_explorer(path: &std::path::Path) {
    #[cfg(target_os = "windows")]
    let done = std::process::Command::new("explorer")
        .arg(path)
        .spawn()
        .is_ok();
    #[cfg(not(target_os = "windows"))]
    let done = std::process::Command::new("xdg-open")
        .arg(path)
        .spawn()
        .is_ok();
    if !done {
        eprintln!("[UI] не удалось открыть папку {}", path.display());
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

//! Операторское приложение (ADR-016): стрим с борта + захват цели
//! двойным кликом + АРМ/СТОП наведения.
//!
//! Сетевая роль — приёмник: борт сам подключается (исходящий TCP,
//! NPU-quirk ядра). Видео — push MJPEG на :9000, управление — JSON-строки
//! на :9010 (default, меняется флагом --control-port).

mod net;
mod style;

use eframe::egui;
use egui::{Color32, ColorImage, Pos2, Rect, Sense, Stroke, TextureHandle, Vec2};

use net::{NetState, UiCommand};
use style::{
    action_btn, bold, chip, counter, top_btn, ACQUIRE_BLUE, ARM_PLAQUE, ARM_RED, ARM_RED_DIM,
    BG_WELL, CONFIRM, CYAN, DANGER, DANGER_ACTIVE, FAIL, LOST_RED, OK, REC_RED, TEXT, TEXT_DIM,
    TOL_AMBER, TOL_GREEN, TRACK_GREEN, W_ARM, W_STOP, W_UNLOCK, WARN,
};

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
    /// Режим с последней смены (таймеры удержания/потери на экране).
    last_mode: String,
    mode_since: std::time::Instant,
    lost_since: Option<std::time::Instant>,
    /// Начало текущей записи (таймер REC на экране).
    rec_since: Option<std::time::Instant>,
    /// Цифровой зум видео (×1..×4, колесо мыши, центр кадра).
    zoom: f32,
    /// Последний установленный заголовок окна (не слать команду зря).
    last_title: String,
    /// Зарезервированная ширина кнопки ЗАПИСЬ/СТОП (по максимальной
    /// метке — счётчики не меняют ширину кнопки, соседи не прыгают).
    rec_btn_w: f32,
    /// Зарезервированная ширина текста REC-бейджа на видео («● REC 888:88»).
    rec_badge_w: f32,
}

impl OperatorApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        // Дизайн-система: PT Sans + DejaVu (значки), палитра, скругления.
        style::install(&cc.egui_ctx);
        // Резерв ширин (по максимальным меткам) меряется лениво на первом
        // кадре: до Context::run() шрифты недоступны (паника egui).
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
            last_mode: String::new(),
            mode_since: std::time::Instant::now(),
            lost_since: None,
            rec_since: None,
            zoom: 1.0,
            last_title: String::new(),
            rec_btn_w: 0.0,
            rec_badge_w: 0.0,
        }
    }

    /// Ленивый резерв ширин (первый кадр): шрифты меряются через ctx,
    /// в конструкторе это паникует («No fonts available until Context::run»).
    fn reserve_widths(&mut self, ctx: &egui::Context) {
        if self.rec_btn_w > 0.0 {
            return;
        }
        // Кнопка ЗАПИСЬ/СТОП: максимальная метка + внутренние отступы +
        // запас. Кнопка не меняет ширину с ростом счётчиков и при
        // переключении старт/стоп — соседи не прыгают.
        self.rec_btn_w =
            ctx.fonts(|f| f.layout_job(rec_stop_label_max()).rect.width()) + 24.0;
        // REC-бейдж на видео: «● REC 888:88» — бокс неподвижен от старта
        // записи до конца (мигает только цвет точки, цифры моноширинные).
        self.rec_badge_w = ctx.fonts(|f| {
            f.layout_job(job(&[
                ("● ".into(), egui::FontId::proportional(18.0), REC_RED),
                ("REC 888:88".into(), egui::FontId::monospace(16.0), REC_RED),
            ]))
            .rect
            .width()
        });
    }

    /// Каталог записей: рядом с exe (dist-папка/ярлык держат WorkingDirectory).
    fn records_dir() -> std::path::PathBuf {
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("records")))
            .unwrap_or_else(|| std::path::PathBuf::from("records"))
    }

    /// Экранная точка → координаты кадра (letterbox + зум вокруг центра).
    fn screen_to_frame(&self, p: Pos2) -> Option<(f32, f32)> {
        let r = self.video_rect?;
        let (fw, fh) = (self.frame_wh.0 as f32, self.frame_wh.1 as f32);
        let scale = (r.width() / fw).min(r.height() / fh);
        let vw = fw * scale;
        let vh = fh * scale;
        let cx = r.left() + (r.width() - vw) / 2.0 + vw / 2.0;
        let cy = r.top() + (r.height() - vh) / 2.0 + vh / 2.0;
        let fx = (p.x - cx) / (scale * self.zoom) + fw / 2.0;
        let fy = (p.y - cy) / (scale * self.zoom) + fh / 2.0;
        let (hw, hh) = (fw / (2.0 * self.zoom), fh / (2.0 * self.zoom));
        (fx >= fw / 2.0 - hw
            && fy >= fh / 2.0 - hh
            && fx <= fw / 2.0 + hw
            && fy <= fh / 2.0 + hh)
            .then_some((fx, fy))
    }

    fn frame_to_screen(&self, x: f32, y: f32) -> Option<Pos2> {
        let r = self.video_rect?;
        let (fw, fh) = (self.frame_wh.0 as f32, self.frame_wh.1 as f32);
        let scale = (r.width() / fw).min(r.height() / fh);
        let vw = fw * scale;
        let vh = fh * scale;
        let cx = r.left() + (r.width() - vw) / 2.0 + vw / 2.0;
        let cy = r.top() + (r.height() - vh) / 2.0 + vh / 2.0;
        Some(Pos2::new(
            cx + (x - fw / 2.0) * scale * self.zoom,
            cy + (y - fh / 2.0) * scale * self.zoom,
        ))
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
        self.reserve_widths(ctx);
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
        // Свежесть сигнала: TCP «жив», а кадры не идут (тихая смерть линка)
        // — запись в этом состоянии пишется пустой. Порог 2 с.
        let signal_stale = video_ok
            && self.net.frame_age_secs().is_some_and(|a| a > 2.0);

        // Таймеры оверлеев: смена режима и старт/стоп записи.
        if let Some(s) = status.as_ref() {
            if s.mode != self.last_mode {
                self.last_mode = s.mode.clone();
                self.mode_since = std::time::Instant::now();
                self.lost_since = (s.mode == "LOST").then(std::time::Instant::now);
            }
        }
        let recording = self.net.recorder().is_recording();
        // Фиксируем момент СТАРТА записи (переход не-идёт → идёт), иначе
        // таймер «REC mm:ss» на видео стоял на 00:00 вечно.
        if recording {
            if self.rec_since.is_none() {
                self.rec_since = Some(std::time::Instant::now());
            }
        } else {
            self.rec_since = None;
        }

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
        // X — снять захват (как кнопка «× СНЯТЬ ЗАХВАТ», только с клавиатуры).
        if ctx.input(|i| i.key_pressed(egui::Key::X)) && ctl_ok {
            let engaged = matches!(
                status.as_ref().map(|s| s.mode.as_str()),
                Some("TRACK") | Some("ACQUIRE") | Some("LOST")
            );
            if engaged {
                self.net.send(UiCommand::Unlock);
            }
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

        // Верхняя панель: связь + счётчики + управление записью
        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                dot(ui, video_ok, "видео");
                dot(ui, ctl_ok, "управление");
                ui.separator();
                ui.label(format!(
                    "видео {:.0} FPS · UI {:.0} FPS",
                    self.net.video_fps(),
                    self.ui_fps
                ));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add(top_btn("Во весь экран (F)")).clicked() {
                        Self::toggle_fullscreen(ctx);
                    }
                    if ui.add(top_btn("Папка записей")).clicked() {
                        open_in_explorer(&Self::records_dir());
                    }
                    ui.separator();
                    // ЗАПИСЬ стрима в AVI/M-JPEG (открывается любым плеером)
                    let rec = self.net.recorder();
                    if rec.is_recording() {
                        let st = rec.stats();
                        let secs = st.started.map(|t| t.elapsed().as_secs()).unwrap_or(0);
                        let stale = !video_ok || signal_stale;
                        // Ширина зарезервирована по максимуму (rec_btn_w):
                        // таймер/мегабайты не растягивают кнопку. Потеря
                        // сигнала — янтарная заливка вместо суффикса (суффикс
                        // менял бы ширину), причина — в подсказке.
                        let mut hover = format!(
                            "{} кадров · сброшено {} (диск не успевал)",
                            st.frames, st.dropped
                        );
                        if stale {
                            hover.push_str(" · НЕТ СИГНАЛА: кадры не приходят");
                        }
                        let btn = egui::Button::new(rec_stop_label(secs, st.bytes))
                            .fill(if stale { style::REC_STALE } else { DANGER })
                            .min_size(egui::vec2(self.rec_btn_w, style::BTN_H_TOP));
                        if ui.add(btn).on_hover_text(hover).clicked() {
                            self.toggle_recording();
                        }
                    } else {
                        let btn = egui::Button::new(bold("● ЗАПИСЬ (R)").size(14.0))
                            .min_size(egui::vec2(self.rec_btn_w, style::BTN_H_TOP));
                        let resp = ui.add_enabled(video_ok, btn);
                        if resp.clicked() {
                            self.toggle_recording();
                        } else if !video_ok {
                            resp.on_disabled_hover_text(
                                "нет видео от борта — запись невозможна; \
                                 проверьте, что борд запущен с --ui и порт 9000 свободен",
                            );
                        }
                    }
                });
            });
            ui.add_space(4.0);
        });

        // Нижняя панель: приборы + действия
        egui::TopBottomPanel::bottom("bottom").show(ctx, |ui| {
            ui.add_space(4.0);
            // Приборный ряд: режим-лампа + score-градусник + fps/e2e с
            // цветовой кодировкой по порогам + компактный FC. Зелёное =
            // норма, янтарь = внимание, красное = проблема — читается
            // одним взглядом без разбора текста. Счётчики — моноширинно
            // (цифры не меняют ширину, ничего не прыгает).
            let (mode_col, mode_txt) = match status.as_ref().map(|s| s.mode.as_str()) {
                Some("TRACK") => (TRACK_GREEN, "TRACK"),
                Some("ACQUIRE") => (ACQUIRE_BLUE, "ACQUIRE"),
                Some("LOST") => (LOST_RED, "LOST"),
                Some("IDLE") => (Color32::GRAY, "ОЖИДАНИЕ"),
                _ => (Color32::GRAY, "—"),
            };
            // Почему FC не вооружится (ADR-021): считаем один раз — чип в
            // приборе, полный список в «стенд»-секции.
            let blockers = status
                .as_ref()
                .filter(|s| !s.armed)
                .and_then(|s| s.fc.as_ref())
                .map(|fc| commander::msp::arming_disable_names(fc.flags))
                .unwrap_or_default();
            ui.horizontal(|ui| {
                ui.colored_label(mode_col, bold(mode_txt).size(20.0));
                if let Some(s) = &status {
                    let sc = s.score.clamp(0.0, 1.0);
                    let scol = if sc >= 0.5 { OK } else if sc >= 0.3 { WARN } else { FAIL };
                    ui.add(
                        egui::ProgressBar::new(sc)
                            .desired_width(96.0)
                            .text(format!("score {:.2}", s.score))
                            .fill(scol),
                    );
                    let fcol = if s.fps >= 50.0 { OK } else if s.fps >= 40.0 { WARN } else { FAIL };
                    counter(ui, fcol, format!("{:.0} fps", s.fps));
                    let ecol = if s.e2e_ms < 6.0 { OK } else if s.e2e_ms < 12.0 { WARN } else { FAIL };
                    counter(ui, ecol, format!("e2e {:.1} мс", s.e2e_ms));
                    counter(ui, TEXT_DIM, format!("дет {} · кадр {}", s.dets.len(), s.frame_seq));
                } else if ctl_ok {
                    ui.weak("нет данных от борта");
                } else {
                    ui.weak("канал управления потерян");
                }
                // Компактный FC: ✓ связь + ✓ RC-поток.
                match status.as_ref().and_then(|s| s.fc.as_ref()) {
                    Some(fc) if fc.online && fc.rx_ok => {
                        chip(ui, egui::RichText::new("FC ✓RC").size(13.0), OK);
                    }
                    Some(fc) if fc.online => {
                        chip(ui, egui::RichText::new("FC ✓·RC ✗").size(13.0), WARN);
                    }
                    Some(_) => {
                        chip(ui, egui::RichText::new("FC ✗").size(13.0), FAIL);
                    }
                    None => {}
                }
                if !blockers.is_empty() {
                    let extra = blockers.len() - 1;
                    let t = if extra > 0 {
                        format!("АРМ-блок: {} (+{extra})", blockers[0])
                    } else {
                        format!("АРМ-блок: {}", blockers[0])
                    };
                    ui.colored_label(WARN, egui::RichText::new(t).size(12.0));
                }
                // Возраст данных: зависший борт виден сразу
                if let Some(age) = status_age {
                    if age > 2.0 {
                        ui.label(
                            egui::RichText::new(format!("данные {age:.0} с назад"))
                                .size(14.0)
                                .color(WARN),
                        );
                    }
                }
            });
            // Стенд-секция: детали FC (полное состояние + эхо каналов
            // MSP_RC + полный список блокировок арма). В поле свёрнута.
            // Внутри — «воздух»: полоски шире своего текста (при узких
            // подписи «ARM 1500» вылезали на соседние полоски) и
            // вертикальные отступы между строками.
            egui::CollapsingHeader::new("стенд: FC · RC-эхо · АРМ-блок")
                .default_open(false)
                .show(ui, |ui| {
            ui.add_space(6.0);
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
                                .color(OK),
                        );
                    } else {
                        ui.label(
                            egui::RichText::new("FC: СВЯЗЬ ОК, НО RC НЕ ВИДИТ (RXLOSS)")
                                .size(12.0)
                                .strong()
                                .color(WARN),
                        );
                    }
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        const LABELS: [&str; 8] = ["R", "P", "T", "Y", "ARM", "A2", "A3", "A4"];
                        for (i, v) in fc.ch.iter().take(6).enumerate() {
                            let frac = ((*v as f32 - 1000.0) / 1000.0).clamp(0.0, 1.0);
                            let armed_ch = i == 4 && *v > 1700;
                            let bar = egui::ProgressBar::new(frac)
                                .desired_width(78.0)
                                .text(
                                    egui::RichText::new(format!("{} {}", LABELS[i], v))
                                        .monospace()
                                        .size(12.0),
                                )
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
                            ui.add_space(6.0);
                            ui.label(
                                egui::RichText::new(text)
                                    .size(12.0)
                                    .color(WARN),
                            );
                        }
                    }
                }
                Some(_) => {
                    ui.label(
                        egui::RichText::new("FC: НЕ ОТВЕЧАЕТ (нет телеметрии)")
                            .size(12.0)
                            .color(FAIL),
                    );
                }
                None => {}
            }
            ui.add_space(4.0);
                });
            ui.add_space(4.0);
            // Кнопочный ряд: единая высота (48 px) и выверенные пропорции —
            // СТОП крупнее прочих, АРМ-подтверждение шире (длинный текст).
            ui.horizontal(|ui| {
                // Состояние наведения — чип, крупно и однозначно.
                let armed = status.as_ref().map(|s| s.armed).unwrap_or(false);
                if armed {
                    chip(
                        ui,
                        bold("● НАВЕДЕНИЕ РАЗРЕШЕНО").size(14.0),
                        Color32::from_rgb(230, 60, 60),
                    );
                } else {
                    chip(
                        ui,
                        egui::RichText::new("○ наведение запрещено").size(14.0),
                        Color32::from_rgb(120, 160, 120),
                    );
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // СТОП — самая заметная кнопка пульта (всегда доступна)
                    if ui
                        .add(action_btn("СТОП (Esc)", 20.0, Some(DANGER), W_STOP))
                        .clicked()
                    {
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
                    let unl = action_btn("× СНЯТЬ ЗАХВАТ", 15.0, None, W_UNLOCK);
                    if ui.add_enabled(track_engaged, unl).clicked() {
                        self.net.send(UiCommand::Unlock);
                    }
                    // АРМ — двухшаговое подтверждение (безопасность):
                    // первый клик только «заряжает» кнопку, второй включает.
                    if armed {
                        let off = action_btn(
                            "● АРМ ВКЛ — выключить",
                            16.0,
                            Some(DANGER_ACTIVE),
                            W_ARM,
                        );
                        if ui.add(off).clicked() {
                            self.net.send(UiCommand::Arm { on: false });
                        }
                    } else if self.arm_confirm {
                        let yes =
                            action_btn("ТОЧНО → РАЗРЕШИТЬ", 16.0, Some(CONFIRM), W_ARM);
                        let resp = ui.add(yes);
                        if resp.clicked() {
                            self.net.send(UiCommand::Arm { on: true });
                            self.arm_confirm = false;
                            self.arm_confirm_at = None;
                        } else if let Some(t0) = self.arm_confirm_at {
                            // Подтверждение гаснет через 4 с — показать
                            // оператору, сколько времени на решение осталось.
                            const ARM_CONFIRM_S: f32 = 4.0;
                            let left =
                                (ARM_CONFIRM_S - t0.elapsed().as_secs_f32()).max(0.0);
                            let r = resp.rect;
                            let w = r.width() * (left / ARM_CONFIRM_S);
                            ui.painter().rect_filled(
                                egui::Rect::from_min_size(
                                    r.left_bottom() + egui::vec2(0.0, 3.0),
                                    egui::vec2(w, 3.0),
                                ),
                                0.0,
                                TOL_AMBER,
                            );
                        }
                    } else {
                        let arm = action_btn("АРМ (2 клика)", 16.0, None, W_ARM);
                        if ui.add(arm).clicked() {
                            self.arm_confirm = true;
                            self.arm_confirm_at = Some(std::time::Instant::now());
                        }
                    }
                });
            });
            // Зарезервированная строка транзиентных сообщений: высота
            // постоянна — панель не «дёргается», когда сообщения приходят
            // и уходят. Приоритет: ошибка записи > нет связи > подсказка
            // подтверждения АРМ > путь сохранённой записи.
            ui.set_min_height(style::MSG_ROW_H);
            ui.horizontal(|ui| {
                if let Some((e, t)) = &self.rec_error {
                    if t.elapsed().as_secs_f32() < 5.0 {
                        ui.label(
                            egui::RichText::new(format!("ошибка записи: {e}"))
                                .size(13.5)
                                .color(FAIL),
                        );
                    } else {
                        self.rec_error = None;
                    }
                } else if let Some(t) = &self.no_link_flash {
                    if t.elapsed().as_secs_f32() < 3.0 {
                        ui.label(
                            egui::RichText::new("нет связи с бортом — команда не отправлена")
                                .size(13.5)
                                .color(WARN),
                        );
                    } else {
                        self.no_link_flash = None;
                    }
                } else if self.arm_confirm {
                    // Автосброс 4 с — не даём «заряженной» кнопке висеть
                    // бесконечно.
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
                            .size(13.5)
                            .color(WARN),
                        );
                    }
                } else if let Some((path, t)) = &self.last_rec {
                    if t.elapsed().as_secs_f32() < 10.0 {
                        // клик по пути — скопировать (папка открыается
                        // кнопкой «Папка записей» сверху)
                        if ui
                            .label(
                                egui::RichText::new(format!(
                                    "запись сохранена: {path} (клик — копировать)"
                                ))
                                .size(13.5)
                                .color(OK),
                            )
                            .clicked()
                        {
                            ctx.copy_text(path.clone());
                        }
                    } else {
                        self.last_rec = None;
                    }
                }
            });
            ui.add_space(2.0);
        });

        // Центр: видео
        egui::CentralPanel::default().show(ctx, |ui| {
            let avail = ui.available_size();
            let (rect, resp) = ui.allocate_exact_size(avail, Sense::click());
            self.video_rect = Some(rect);
            // Зум колесом (×1..×4, вокруг центра кадра): разглядеть мелкую
            // цель. Клик-захват работает и в зуме (маппинг учитывает его).
            let scroll = ctx.input(|i| i.smooth_scroll_delta.y);
            if resp.hovered() && scroll != 0.0 {
                self.zoom = (self.zoom * if scroll > 0.0 { 1.15 } else { 1.0 / 1.15 })
                    .clamp(1.0, 4.0);
            }
            // letterbox-подгонка текстуры под фактический размер кадра
            let (scale, vrect) = self.video_geom(rect);
            ui.painter().rect_filled(rect, 0.0, BG_WELL);
            if video_ok {
                let half = 0.5 - 0.5 / self.zoom;
                let uv = Rect::from_min_max(
                    Pos2::new(half, half),
                    Pos2::new(1.0 - half, 1.0 - half),
                );
                ui.painter().image(self.texture.id(), vrect, uv, Color32::WHITE);
            } else {
                // Ошибки bind (порт занят второй копией пульта) важнее
                // «ждём борт» — без них REC молча мёртв, а причина в консоли.
                let bind_errors = self.net.bind_errors();
                let (text, col) = if bind_errors.is_empty() {
                    (
                        "ждём борт…\nзапустите: synergy --ui <этот-хост>:9010".to_string(),
                        Color32::GRAY,
                    )
                } else {
                    (
                        format!("НЕ ЗАПУСТИЛСЯ ПРИЁМ\n{}", bind_errors.join("\n")),
                        Color32::from_rgb(255, 170, 0),
                    )
                };
                ui.painter().text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    text,
                    egui::FontId::proportional(20.0),
                    col,
                );
            }

            // оверлеи: детекции (красные), цель (зелёная/цвет режима)
            if let Some(s) = &status {
                for d in &s.dets {
                    if let Some(p) = self.frame_to_screen(d.0, d.1) {
                        let wh = Pos2::new(
                            p.x + d.2 * scale * self.zoom,
                            p.y + d.3 * scale * self.zoom,
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
                        "TRACK" => TRACK_GREEN,
                        "ACQUIRE" => ACQUIRE_BLUE,
                        _ => LOST_RED,
                    };
                    if let Some(p) = self.frame_to_screen(b[0] as f32, b[1] as f32) {
                        let wh = Pos2::new(
                            p.x + b[2] as f32 * scale * self.zoom,
                            p.y + b[3] as f32 * scale * self.zoom,
                        );
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

            // вспышка LOCK SENT (ниже плашки АРМ — обе по центру сверху)
            if let Some(t) = self.lock_flash_ms {
                if t.elapsed().as_millis() < 600 {
                    ui.painter().text(
                        vrect.center_top() + Vec2::new(0.0, 56.0),
                        egui::Align2::CENTER_CENTER,
                        "ЗАХВАТ ОТПРАВЛЕН",
                        egui::FontId::proportional(20.0),
                        Color32::from_rgb(120, 220, 255),
                    );
                } else {
                    self.lock_flash_ms = None;
                }
            }

            // === Оверлеи оператора (UX P1): прицел+допуск, режим+таймеры,
            // REC, АРМ-рамка. Цвета выбраны уникальными — их ищет
            // tools/ui_visual_test.py (пиксельные проверки фич). ===
            if video_ok {
                let egui_t = ui.ctx().input(|i| i.time) as f32;

                // 1) Прицел: крест с зазором в центре кадра + зона
                //    допуска ±30 px (критерий удержания, фаза D).
                let c = vrect.center();
                let cyan = CYAN;
                for seg in [
                    (Pos2::new(c.x - 18.0, c.y), Pos2::new(c.x - 7.0, c.y)),
                    (Pos2::new(c.x + 7.0, c.y), Pos2::new(c.x + 18.0, c.y)),
                    (Pos2::new(c.x, c.y - 18.0), Pos2::new(c.x, c.y - 7.0)),
                    (Pos2::new(c.x, c.y + 7.0), Pos2::new(c.x, c.y + 18.0)),
                ] {
                    ui.painter().line_segment([seg.0, seg.1], Stroke::new(2.0_f32, cyan));
                }
                let in_tol = status.as_ref().and_then(|s| s.box_xywh).map(|b| {
                    let (bx, by) =
                        (b[0] as f32 + b[2] as f32 / 2.0, b[1] as f32 + b[3] as f32 / 2.0);
                    let (fcx, fcy) =
                        (self.frame_wh.0 as f32 / 2.0, self.frame_wh.1 as f32 / 2.0);
                    ((bx - fcx).powi(2) + (by - fcy).powi(2)).sqrt() <= 30.0
                });
                let tol_col = if in_tol == Some(true) {
                    TOL_GREEN // цель в допуске
                } else {
                    TOL_AMBER // вне допуска / цели нет
                };
                ui.painter().circle_stroke(
                    c,
                    30.0 * scale * self.zoom,
                    Stroke::new(2.0_f32, tol_col),
                );

                // 2) Бейдж режима + таймеры удержания/потери (периферийное
                // зрение оператора). Нижние углы кадра: верхние заняты OSD
                // борта. Числа — моноширинные с фиксированным форматом
                // ({:>5.1}): ширина бейджа постоянна, бокс не дёргается
                // при смене цифр (рост только на 16.7-й минуте удержания).
                let prop22 = || egui::FontId::proportional(22.0);
                let mono20 = || egui::FontId::monospace(20.0);
                let mode_job = |label: &str, secs: f32, col: Color32| {
                    job(&[
                        (format!("{label} · "), prop22(), col),
                        (format!("{secs:>5.1}"), mono20(), col),
                        (" с".into(), prop22(), col),
                    ])
                };
                let (mcol, text) = match status.as_ref().map(|s| s.mode.as_str()) {
                    Some("TRACK") => (
                        TRACK_GREEN,
                        mode_job("TRACK", self.mode_since.elapsed().as_secs_f32(), TRACK_GREEN),
                    ),
                    Some("ACQUIRE") => (
                        ACQUIRE_BLUE,
                        mode_job(
                            "ACQUIRE",
                            self.mode_since.elapsed().as_secs_f32(),
                            ACQUIRE_BLUE,
                        ),
                    ),
                    Some("LOST") => {
                        let t = self
                            .lost_since
                            .map(|i| i.elapsed().as_secs_f32())
                            .unwrap_or(0.0);
                        (LOST_RED, mode_job("ПОТЕРЯН", t, LOST_RED))
                    }
                    Some("IDLE") => (
                        Color32::from_rgb(160, 160, 160),
                        job(&[("ЗАХВАТ СНЯТ".into(), prop22(), Color32::from_rgb(160, 160, 160))]),
                    ),
                    _ => (
                        Color32::from_rgb(160, 160, 160),
                        job(&[("нет данных".into(), prop22(), Color32::from_rgb(160, 160, 160))]),
                    ),
                };
                let anchor = vrect.left_bottom() + Vec2::new(10.0, -10.0);
                // min_w — зарезервированная ширина бокса (по максимуму):
                // содержимое не растягивает/не сжимает рамку.
                let badge = |anchor: Pos2,
                             align: egui::Align2,
                             text: egui::text::LayoutJob,
                             col: Color32,
                             min_w: f32| {
                    let galley = ui.painter().layout_job(text);
                    let mut tr = align.anchor_size(anchor, galley.size());
                    if tr.width() < min_w {
                        let d = min_w - tr.width();
                        // расширяем в сторону свободного края (якорь на месте)
                        if align == egui::Align2::LEFT_BOTTOM {
                            tr.max.x += d;
                        } else {
                            tr.min.x -= d;
                        }
                    }
                    ui.painter().rect_filled(
                        tr.expand(6.0),
                        4.0,
                        Color32::from_rgba_premultiplied(15, 15, 18, 215),
                    );
                    ui.painter()
                        .rect_stroke(tr.expand(6.0), 4.0, Stroke::new(1.5_f32, col));
                    ui.painter().galley(tr.min, galley, col);
                    tr
                };
                let tr_mode = badge(anchor, egui::Align2::LEFT_BOTTOM, text, mcol, 0.0);

                // 2в) Индикатор зума (только когда включён) — правый нижний.
                if self.zoom > 1.01 {
                    badge(
                        vrect.right_bottom() + Vec2::new(-10.0, -10.0),
                        egui::Align2::RIGHT_BOTTOM,
                        job(&[(
                            format!("×{:.1}", self.zoom),
                            egui::FontId::proportional(18.0),
                            cyan,
                        )]),
                        cyan,
                        0.0,
                    );
                }

                // 3) REC-таймер НАД бейджем режима (стек снизу-слева).
                // Точка НЕ убирается (иначе бокс прыгает каждые полсекунды) —
                // мигает ЦВЕТОМ; таймер моноширинный; ширина бейджа
                // зарезервирована по «● REC 888:88» — бокс неподвижен.
                if let Some(rs) = self.rec_since {
                    let blink = (egui_t * 2.0).fract() < 0.65;
                    let e = rs.elapsed().as_secs();
                    let dot_col = if blink { REC_RED } else { Color32::TRANSPARENT };
                    let text = job(&[
                        ("● ".into(), egui::FontId::proportional(18.0), dot_col),
                        (
                            format!("REC {:02}:{:02}", (e / 60) % 100, e % 60),
                            egui::FontId::monospace(16.0),
                            REC_RED,
                        ),
                    ]);
                    badge(
                        anchor + Vec2::new(0.0, -(tr_mode.height() + 8.0)),
                        egui::Align2::LEFT_BOTTOM,
                        text,
                        REC_RED,
                        self.rec_badge_w,
                    );
                }

                // 4) АРМ: рамка кадра + плашка (видно боковым зрением).
                if status.as_ref().is_some_and(|s| s.armed) {
                    let blink = (egui_t * 2.0).fract() < 0.6;
                    let red = if blink { ARM_RED } else { ARM_RED_DIM };
                    ui.painter()
                        .rect_stroke(vrect.shrink(3.0), 2.0, Stroke::new(6.0_f32, red));
                    let a3 = vrect.center_top() + Vec2::new(0.0, 26.0);
                    let tr3 = ui.painter().text(
                        a3,
                        egui::Align2::CENTER_CENTER,
                        "НАВЕДЕНИЕ АКТИВНО",
                        egui::FontId::proportional(20.0),
                        Color32::WHITE,
                    );
                    ui.painter().rect_filled(
                        tr3.expand(9.0),
                        4.0,
                        // полупрозрачный ARM_PLAQUE (тест ищет именно 150,25,25)
                        Color32::from_rgba_premultiplied(
                            ARM_PLAQUE.r(),
                            ARM_PLAQUE.g(),
                            ARM_PLAQUE.b(),
                            235,
                        ),
                    );
                    ui.painter().text(
                        a3,
                        egui::Align2::CENTER_CENTER,
                        "НАВЕДЕНИЕ АКТИВНО",
                        egui::FontId::proportional(20.0),
                        Color32::WHITE,
                    );
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
    let col = if ok { TRACK_GREEN } else { LOST_RED };
    let (pos, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), Sense::hover());
    ui.painter()
        .circle_filled(pos.center(), 5.0, col);
    ui.label(label).on_hover_text(if ok { "подключено" } else { "нет связи" });
}

/// Текст из сегментов с разными шрифтами: подписи — пропорциональный
/// шрифт, ЧИСЛА — моноширинный: любая комбинация цифр одной длины даёт
/// одну и ту же ширину, счётчики не «дёргают» бокс.
fn job(segments: &[(String, egui::FontId, Color32)]) -> egui::text::LayoutJob {
    let mut j = egui::text::LayoutJob::default();
    for (text, id, col) in segments {
        j.append(text, 0.0, egui::TextFormat::simple(id.clone(), *col));
    }
    j
}

/// Метка кнопки записи: таймер и размер моноширинные, размер — формат
/// фиксированной ширины ({:>5.1}, после 999.9 МБ → ГБ), минуты — резерв
/// до 999:59. Кнопка НЕ меняет ширину ни с ростом счётчиков, ни при
/// переключении СТОП ↔ ЗАПИСЬ (соседи сверху не прыгают).
fn rec_stop_label(secs: u64, bytes: u64) -> egui::text::LayoutJob {
    let (mm, ss) = (secs / 60, secs % 60);
    let mb = bytes as f32 / 1e6;
    let (val, unit) = if mb < 1000.0 {
        (mb, "МБ")
    } else {
        (bytes as f32 / 1e9, "ГБ")
    };
    let bold = egui::FontId::new(14.0, egui::FontFamily::Name(style::BOLD_FAMILY.into()));
    let mono = egui::FontId::monospace(13.0);
    let b = |t: &str| (t.to_string(), bold.clone(), TEXT);
    let m = |t: String| (t, mono.clone(), TEXT);
    job(&[
        b("■ СТОП · "),
        m(format!("{mm:02}:{ss:02}")),
        b(" · "),
        m(format!("{val:>5.1} {unit}")),
    ])
}

/// Максимальная метка кнопки записи (для измерения резерва ширины).
fn rec_stop_label_max() -> egui::text::LayoutJob {
    rec_stop_label(999 * 60 + 59, 999_900_000)
}

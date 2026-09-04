//! synergyAutotargeting — гибридный трекинг (вариант C):
//! детекция YOLOv8 на NPU раз в N кадров + NanoTrack на CPU каждый кадр.
//! Железо: Radxa ROCK 5A (RK3588S) + USB-камера Arducam.

mod config;
mod control;
mod diag;
mod osd;
mod synthetic;

use std::io::Write as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc as std_mpsc;
use std::time::{Duration, Instant};

/// Глобальный флаг остановки: SIGINT/SIGTERM → чистый STREAMOFF и итоги.
static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_sig: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
}

fn install_signal_handlers() {
    unsafe {
        libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
    }
}

use anyhow::{bail, Context, Result};
use clap::Parser;
use common::Detection;
#[cfg(target_os = "linux")]
use common::{Frame, PixelFormat};
use nano_track::imgops::Img;
use pipeline::{HybridConfig, HybridTracker, Mode};
use serde::Serialize;

use crate::config::AppConfig;

/// Ответ детектор-воркера.
pub struct DetectResult {
    pub detections: Vec<Detection>,
    pub infer_ms: f32,
    pub frame_seq: u64,
}

#[derive(Parser, Debug)]
#[command(name = "synergy", version, about = "Гибридный трекер: детекция каждые N кадров + NanoTrack каждый кадр")]
struct Args {
    /// Путь к config.toml
    #[arg(short, long, default_value = "config.toml")]
    config: String,
    /// Синтетический источник вместо камеры (локальная отладка без железа)
    #[arg(long)]
    synthetic: bool,
    /// Длительность прогона, сек (переопределяет конфиг; 0 = бесконечно)
    #[arg(short, long)]
    duration: Option<u64>,
    /// Каталог вывода
    #[arg(short, long, default_value = None)]
    output: Option<String>,
    /// Демо-детекция (цель-фантом в центре): проверка трекинга
    /// на реальных кадрах без распознаваемого моделью объекта)
    #[arg(long)]
    demo_detect: bool,
    /// Включить живой MJPEG-стрим OSD (http://<ip>:<port>/, см. [stream]).
    #[arg(long)]
    stream: bool,
    /// Push-стрим: борт сам подключается к зрителю (ADDR:PORT, напр.
    /// 192.168.0.174:9000; приёмник — tools/viewer.py). Обходит quirk ядра.
    #[arg(long, value_name = "ADDR")]
    stream_push: Option<String>,
    /// Диагностика нейро-бэкендов трекера (tract ↔ rknn): косинусная
    /// близость выходов на одинаковых кропах, включая проверку layout.
    #[arg(long)]
    diag_nets: bool,
    /// Оффлайн-прогон записи (.mjpg от --record) через весь пайплайн
    /// (L7): детектор+трекер+телеметрия без железа. На борту — с NPU.
    #[arg(long, value_name = "PATH")]
    replay: Option<String>,
    /// Скорость --replay: 1.0 = реальные 30 FPS, 0 = максимально быстро.
    #[arg(long, default_value_t = 1.0)]
    replay_rate: f32,

    /// Диагностический режим: полный сбор данных прогона
    /// (каталог data/runs/…, telemetry/detections/commander/gmc/perf).
    #[arg(long)]
    diag: bool,

    /// Операторский UI: ADDR контрольного канала (напр. 192.168.0.174:9010);
    /// видео-стрим автоматически пушится на тот же хост, порт 9000.
    #[arg(long, value_name = "ADDR")]
    ui: Option<String>,
    /// Тряска камеры в синтетике, px (стенд GMC-стабилизации).
    #[arg(long, default_value_t = 0.0)]
    shake: f32,
    /// Записать стрим OSD в H.264 файл (аппаратный mpph264enc, .mkv).
    #[arg(long, value_name = "PATH")]
    record_h264: Option<String>,
    /// Записать стрим OSD в M-JPEG файл (путь; расширение .mjpg).
    #[arg(long, value_name = "PATH")]
    record: Option<String>,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let args = Args::parse();
    install_signal_handlers();
    #[cfg(feature = "npu")]
    if args.diag_nets {
        return diag_nets();
    }
    let mut cfg = AppConfig::load(&args.config)
        .with_context(|| format!("чтение {}", args.config))?;
    if let Some(d) = args.duration {
        cfg.output.duration_secs = d;
    }
    if let Some(o) = &args.output {
        cfg.output.dir = o.clone();
    }
    if args.stream {
        cfg.stream.enabled = true;
    }
    if let Some(r) = &args.record {
        cfg.stream.record = r.clone();
        cfg.stream.enabled = true; // запись идёт через стрим-энкодер
    }
    if let Some(r) = &args.record_h264 {
        cfg.stream.record_h264 = r.clone();
        cfg.stream.enabled = true;
    }
    if args.shake > 0.0 {
        cfg.synthetic.shake_px = args.shake;
    }
    if args.diag {
        cfg.logging.mode = config::LogMode::Diag;
    }
    if let Some(addr) = &args.ui {
        cfg.control.ui_addr = addr.clone();
        // видео к UI — на тот же хост, стандартный порт приёмника
        if let Some(host) = addr.rsplit_once(':').map(|(h, _)| h) {
            cfg.stream.push_to = format!("{host}:9000");
        }
    }
    if let Some(addr) = &args.stream_push {
        cfg.stream.push_to = addr.clone();
        cfg.stream.enabled = true; // push включает стрим-контекст целиком
    }
    std::fs::create_dir_all(&cfg.output.dir).ok();

    let mut run = Runner::new(cfg, args.synthetic, args.demo_detect)?;
    run.replay = args.replay.clone();
    run.replay_rate = args.replay_rate;
    run.run()
}

/// Телеметрическая строка (JSONL).
#[derive(Serialize)]
struct TelemetryLine {
    ts_ms: u64,
    frame_seq: u64,
    mode: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    x: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    y: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    w: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    h: Option<i32>,
    score: f32,
    track_ms: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    det_ms: Option<f32>,
    fps: f32,
    /// Полная латентность кадр→бокс (получение кадра → трек готов), мс.
    e2e_ms: f32,
}

struct RunStats {
    frames: u64,
    detections_run: u64,
    detections_hits: u64,
    reacquires: u64,
    track_us_total: u128,
    detect_us_total: u128,
    /// Распределение режимов.
    tracking_frames: u64,
    lost_frames: u64,
}

struct Runner {
    cfg: AppConfig,
    synthetic: bool,
    demo_detect: bool,
    /// Оффлайн-прогон записи (L7): путь и множитель скорости (0 = макс).
    replay: Option<String>,
    replay_rate: f32,
}

/// Контур наведения (фаза D): закон + транспорт, отправка по rate_hz.
struct CommanderCtx {
    law: commander::AimLaw,
    link: Box<dyn commander::AimLink>,
    period: Duration,
    last_sent: Instant,
    sent: u64,
    sim: Option<commander::PlatformSim>,
    /// Разрешение наведения от оператора (АРМ); выкл — стики в центр.
    pub armed: bool,
    /// Диагностика последнего тика (L3): err, vel, lead, каналы.
    pub last_logged: Option<((f32, f32), (f32, f32), (f32, f32), [u16; 16])>,
}

impl CommanderCtx {
    fn new(cfg: &AppConfig) -> Result<Self> {
        let c = &cfg.commander;
        let axis = |reverse: bool| commander::AxisParams {
            kp: c.kp,
            ki: c.ki,
            kd: c.kd,
            deadband_px: c.deadband_px,
            slew_us: c.slew_us,
            reverse,
        };
        let law = commander::AimLaw::new(commander::AimConfig {
            x: axis(c.reverse_x),
            y: axis(c.reverse_y),
            swap_axes: c.swap_axes,
            throttle_us: c.throttle_us,
            aux1_us: c.aux1_us,
            lead_s: c.lead_s,
            lead_alpha: c.lead_alpha,
            stick_rate_px_s: c.stick_rate_px_s,
        });
        let link: Box<dyn commander::AimLink> = if c.simulate {
            tracing::info!("коммандер: РЕЖИМ СИМУЛЯЦИИ (UART отключён)");
            Box::new(commander::NoopLink::new())
        } else {
            match commander::uart::UartLink::open(&c.device, c.baud) {
                Ok(l) => Box::new(l),
                Err(e) => {
                    tracing::warn!(error = %e, "UART недоступен — команды идут в никуда (noop)");
                    Box::new(commander::NoopLink::new())
                }
            }
        };
        Ok(Self {
            law,
            link,
            period: Duration::from_secs_f32(1.0 / c.rate_hz.max(1) as f32),
            last_sent: Instant::now() - Duration::from_secs(1),
            sent: 0,
            sim: None,
            armed: false,
            last_logged: None,
        })
    }

    /// АРМ/разарм от оператора; при выключении стики уходят в центр.
    pub fn set_armed(&mut self, on: bool) {
        if self.armed && !on {
            let ch = self.law.lost();
            let _ = self.link.send_rc(&ch);
        }
        self.armed = on;
    }

    /// Мгновенный СТОП: центры + разарм (UI/fail-safe).
    pub fn emergency_stop(&mut self) {
        self.set_armed(false);
    }

    /// Такт контура: цель в кадре → RC; потеря → центры.
    fn tick(
        &mut self,
        mode: pipeline::Mode,
        target_px: Option<(f32, f32)>,
        frame: (u32, u32),
    ) {
        let now = Instant::now();
        if now - self.last_sent < self.period {
            return;
        }
        self.last_sent = now;
        if !self.armed {
            return; // не арт: команды не идут (безопасность по умолчанию)
        }
        let dt = self.period.as_secs_f32();
        let ch = match (mode, target_px) {
            (pipeline::Mode::Tracking, Some((x, y))) => self.law.update((x, y), frame, dt),
            _ => self.law.lost(),
        };
        if self.link.send_rc(&ch).is_ok() {
            self.sent += 1;
        }
        self.last_logged = Some((self.law.last_info.0, self.law.last_info.1, self.law.last_info.2, ch));
        if let Some(sim) = &mut self.sim {
            sim.step(&ch, dt);
            tracing::debug!(
                sim_x = sim.pos_x,
                sim_y = sim.pos_y,
                ch0 = ch[0],
                ch1 = ch[1],
                "симуляция платформы"
            );
        }
    }
}

/// Живой стрим OSD: сервер + канал к потоку-энкодеру JPEG + push-выход.
struct StreamCtx {
    server: Option<streaming::MjpegServer>,
    push: Option<std::sync::Arc<streaming::MjpegPusher>>,
    enc_tx: streaming::LatestSender<(Vec<u8>, u32, u32)>,
    frame_div: u32,
    /// Идёт запись стрима в файл (--record): энкодер работает и без зрителей.
    recording: bool,
}

impl StreamCtx {
    /// Кодировать кадр имеет смысл при зрителе или записи.
    fn wanted(&self) -> bool {
        self.recording
            || self.server.as_ref().is_some_and(|s| s.clients() > 0)
            || self.push.as_ref().is_some_and(|p| p.connected())
    }
}

impl Runner {
    fn new(cfg: AppConfig, synthetic: bool, demo_detect: bool) -> Result<Self> {
        Ok(Self {
            cfg,
            synthetic,
            demo_detect,
            replay: None,
            replay_rate: 1.0,
        })
    }

    fn run(&self) -> Result<()> {
        let cfg = self.cfg.clone();
        let started = Instant::now();
        let deadline = if cfg.output.duration_secs > 0 {
            Some(started + Duration::from_secs(cfg.output.duration_secs))
        } else {
            None
        };

        // === Трекер (NanoTrack): tract (CPU) или RKNN (NPU, фаза C) ===
        let nano = match cfg.tracker.backend.trim().to_ascii_lowercase().as_str() {
            #[cfg(feature = "npu")]
            "rknn" => {
                let nets = nano_track::backend_rknn::RknnNets::load(
                    &cfg.tracker.rknn_backbone_z,
                    &cfg.tracker.rknn_backbone_x,
                    &cfg.tracker.rknn_head,
                    cfg.tracker.swap_rb,
                )
                .context("загрузка RKNN-моделей NanoTrack")?;
                tracing::info!("трекер: бэкенд RKNN (NPU)");
                nano_track::NanoTracker::with_nets(Box::new(nets))?
            }
            _ => {
                tracing::info!("трекер: бэкенд tract (CPU)");
                nano_track::NanoTracker::new(
                    &cfg.tracker.backbone_path,
                    &cfg.tracker.backbone_search_path,
                    &cfg.tracker.head_path,
                    cfg.tracker.swap_rb,
                )?
            }
        };
        let hybrid_cfg = HybridConfig {
            detect_every_n: cfg.pipeline.detect_every_n,
            iou_confirm: cfg.pipeline.iou_confirm,
            min_track_score: cfg.tracker.min_track_score,
            lost_patience: cfg.pipeline.lost_patience,
            min_detect_conf: cfg.pipeline.min_detect_conf,
            priority_classes: cfg.pipeline.priority_classes.clone(),
            use_stabilizer: cfg.pipeline.use_stabilizer,
            gmc: cfg.pipeline.gmc,
        };
        let mut hybrid = HybridTracker::new(nano, hybrid_cfg);

        // === Стрим OSD (MJPEG, ADR-009): слушающий сервер и/или push ===
        let stream_ctx = if cfg.stream.enabled
            || !cfg.stream.push_to.is_empty()
        {
            let server = if cfg.stream.enabled {
                let s = streaming::MjpegServer::start(&cfg.stream.bind)
                    .with_context(|| format!("bind {}", cfg.stream.bind))?;
                tracing::info!(
                    port = s.port(),
                    "стрим MJPEG (listen): http://<ip>:{}/ (браузер/VLC)", s.port()
                );
                Some(s)
            } else {
                None
            };
            let push = if !cfg.stream.push_to.is_empty() {
                let p = std::sync::Arc::new(streaming::MjpegPusher::start(&cfg.stream.push_to));
                tracing::info!(addr = %cfg.stream.push_to, "стрим MJPEG (push): подключаюсь к зрителю");
                Some(p)
            } else {
                None
            };
            let (enc_tx, enc_rx) = streaming::latest_channel::<(Vec<u8>, u32, u32)>();
            let quality = cfg.stream.quality;
            let srv = server.clone();
            let psh = push.clone();
            #[cfg(target_os = "linux")]
            let h264 = if cfg.stream.record_h264.is_empty() {
                None
            } else {
                match spawn_h264_gst(&cfg.stream.record_h264) {
                    Ok(pair) => {
                        tracing::info!(path = %cfg.stream.record_h264, "H.264-запись (mpph264enc)");
                        Some(pair)
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "H.264-запись недоступна (gst-launch/mpph264enc?)");
                        None
                    }
                }
            };
            let mut rec_file = if cfg.stream.record.is_empty() {
                None
            } else {
                match std::fs::File::create(&cfg.stream.record) {
                    Ok(f) => {
                        tracing::info!(path = %cfg.stream.record, "запись стрима в файл");
                        Some(f)
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "файл записи недоступен");
                        None
                    }
                }
            };
            // Кодирование — вне бюджетного потока кадра (§8 HARDWARE_TEST_RESULTS).
            #[cfg(target_os = "linux")]
            let (h264_child, mut h264_stdin) = match h264 {
                Some((child, stdin)) => (Some(child), Some(stdin)),
                None => (None, None),
            };
            #[cfg(target_os = "linux")]
            let recording = rec_file.is_some() || h264_stdin.is_some();
            #[cfg(not(target_os = "linux"))]
            let recording = rec_file.is_some();
            let _enc = std::thread::Builder::new()
                .name("mjpeg-encoder".into())
                .spawn(move || {
                    while let Some((rgb, w, h)) = enc_rx.recv() {
                        match encode_jpeg_bytes(&rgb, w, h, quality) {
                            Ok(j) => {
                                if let Some(f) = rec_file.as_mut() {
                                    let _ = f.write_all(&j); // M-JPEG: кадры подряд
                                }
                                #[cfg(target_os = "linux")]
                                if let Some(stdin) = h264_stdin.as_mut() {
                                    let nv12 = rgb24_to_nv12(&rgb, w, h);
                                    if stdin.write_all(&nv12).is_err() {
                                        tracing::warn!("H.264-конвейер оборвался");
                                    }
                                }
                                if let Some(s) = &srv {
                                    if s.clients() > 0 {
                                        s.push_jpeg(j.clone());
                                    }
                                }
                                if let Some(p) = &psh {
                                    p.send(j);
                                }
                            }
                            Err(e) => tracing::warn!(error = %e, "кадр стрима не закодирован"),
                        }
                    }
                })
                .context("spawn mjpeg-encoder")?;
            #[cfg(target_os = "linux")]
            let _h264_child = h264_child; // Child в scope: Drop по завершении run()
            Some(StreamCtx {
                server,
                push,
                enc_tx,
                frame_div: cfg.stream.frame_div,
                recording,
            })
        } else {
            None
        };

        // === Детектор ===
        let (det_req_tx, det_req_rx) = std_mpsc::sync_channel::<(Vec<u8>, u32, u32, u64)>(1);
        let (det_resp_tx, det_resp_rx) = std_mpsc::sync_channel::<Result<DetectResult, String>>(1);
        let det_handle = self.spawn_detector(&cfg, det_req_rx, det_resp_tx)?;

        // === Источник кадров ===
        // Режимы (ADR-017): battle — без записи; diag — каталог прогона.
        let mut diag = match cfg.logging.mode {
            config::LogMode::Diag => diag::DiagSink::open(&cfg)
                .context("создание каталога диагностики")?,
            config::LogMode::Battle => {
                tracing::info!("БОЕВОЙ режим: журналы прогона не пишутся");
                diag::DiagSink::disabled()
            }
        };

        let mut stats = RunStats {
            frames: 0,
            detections_run: 0,
            detections_hits: 0,
            reacquires: 0,
            track_us_total: 0,
            detect_us_total: 0,
            tracking_frames: 0,
            lost_frames: 0,
        };

        // === Канал операторского UI (ADR-016) ===
        let control = if cfg.control.ui_addr.is_empty() {
            None
        } else {
            Some(std::sync::Arc::new(control::ControlLink::start(&cfg.control.ui_addr)))
        };

        // === Коммандер наведения (фаза D) ===
        let mut commander_ctx = if cfg.commander.enabled || cfg.commander.simulate {
            Some(CommanderCtx::new(&cfg)?)
        } else {
            None
        };

        let result = if let Some(path) = self.replay.clone() {
            self.run_replay(
                &path, self.replay_rate, &cfg, &mut hybrid, stream_ctx.as_ref(),
                commander_ctx.as_mut(), control.as_ref(), &det_req_tx, &det_resp_rx,
                &mut stats, &mut diag,
            )
        } else if self.synthetic {
            self.run_synthetic(&cfg, &mut hybrid, stream_ctx.as_ref(), commander_ctx.as_mut(), control.as_ref(), &det_req_tx, &det_resp_rx, &mut stats, &mut diag, deadline)
        } else {
            self.run_camera(&cfg, &mut hybrid, stream_ctx.as_ref(), commander_ctx.as_mut(), control.as_ref(), &det_req_tx, &det_resp_rx, &mut stats, &mut diag, deadline)
        };

        // === Итоги ===
        let elapsed = started.elapsed();
        let fps = stats.frames as f32 / elapsed.as_secs_f32().max(1e-6);
        println!("=== ИТОГ ===");
        println!(
            "кадров: {}, FPS: {fps:.1}, длительность: {:.1}s",
            stats.frames,
            elapsed.as_secs_f32()
        );
        println!(
            "детекций: {} (с целью: {}), реинициализаций трекера: {}",
            stats.detections_run, stats.detections_hits, stats.reacquires
        );
        if stats.frames > 0 {
            println!(
                "средний track: {:.2} мс, кадров в TRACKING: {} ({:.0}%), в LOST: {}",
                stats.track_us_total as f32 / stats.frames as f32 / 1000.0,
                stats.tracking_frames,
                100.0 * stats.tracking_frames as f32 / stats.frames as f32,
                stats.lost_frames
            );
        }
        if stats.detections_run > 0 {
            println!(
                "средний инференс NPU: {:.1} мс",
                stats.detect_us_total as f32 / stats.detections_run as f32 / 1000.0
            );
        }
        // `let _ = det_req_tx;` здесь НЕ роняет канал: паттерн `_` не двигает
        // place-expression (проверено gdb на борту — воркер в recv(), main в
        // join()). Явные drop обоих концов размыкают цикл воркера (ADR-008).
        drop(det_req_tx);
        drop(det_resp_rx);
        if let Some(h) = det_handle {
            let _ = h.join();
        }
        if let Some(c) = &commander_ctx {
            println!("коммандер: {} RC-кадров, закон {} (swap_axes={})", c.sent, if cfg.commander.simulate { "СИМУЛЯЦИЯ" } else { "UART/MSP" }, cfg.commander.swap_axes);
        }
        if let Some(ctl) = &control {
            println!("UI-канал: {} статусов отправлено (связь: {})", ctl.sent_status.load(std::sync::atomic::Ordering::Relaxed), ctl.connected());
        }
        if let Some(st) = &stream_ctx {
            let served = st.server.as_ref().map(|s| s.served_frames()).unwrap_or(0);
            let pushed = st.push.as_ref().map(|p| p.connected()).unwrap_or(false);
            println!("стрим: {served} кадров отдано слушателям (push: {pushed})");
        }
        result
    }

    /// Воркер детектора. На фиче npu — реальный NPU, иначе стаб.
    #[allow(unused_variables)]
    fn spawn_detector(
        &self,
        cfg: &AppConfig,
        req_rx: std_mpsc::Receiver<(Vec<u8>, u32, u32, u64)>,
        resp_tx: std_mpsc::SyncSender<Result<DetectResult, String>>,
    ) -> Result<Option<std::thread::JoinHandle<()>>> {
        #[cfg(feature = "npu")]
        {
            let model_path = cfg.detector.model_path.clone();
            let input = cfg.detector.input_size;
            // diag: пол 0.05 — сырой лог виден весь фон (L2); рабочий
            // порог всё равно применяется в process_frame.
            let conf = match cfg.logging.mode {
                config::LogMode::Diag => cfg.detector.conf_threshold.min(0.01),
                config::LogMode::Battle => cfg.detector.conf_threshold,
            };
            let nms = cfg.detector.nms_threshold;
            let class_names = cfg.detector.class_names.clone();
            let handle = std::thread::Builder::new()
                .name("npu-detector".into())
                .spawn(move || {
                    let mut model = match rknn_sys::RknnModel::load(
                        &model_path,
                        Some((input, input)),
                    ) {
                        Ok(m) => m,
                        Err(e) => {
                            let _ = resp_tx.send(Err(format!("load: {e}")));
                            return;
                        }
                    };
                    let dims = model.output_dims();
                    let decoder = match detector::YoloDecoder::from_output_dims(
                        &dims,
                        detector::DecoderConfig {
                            conf_threshold: conf,
                            nms_threshold: nms,
                            class_names,
                            sigmoid_classes: false,
                        },
                    ) {
                        Some(d) => d,
                        None => {
                            let _ = resp_tx.send(Err(format!(
                                "не удалось определить layout выходов: {dims:?}"
                            )));
                            return;
                        }
                    };
                    if decoder.is_single_head() {
                        tracing::warn!(
                            "SingleHead-модель: int8 даёт conf=0.5 артефакт (ADR-014); используйте 9-веточные модели (bkb)"
                        );
                    }
                    tracing::info!(
                        layout = ?decoder.layout,
                        classes = decoder.num_classes,
                        "декодер YOLOv8 готов"
                    );
                    while let Ok((rgb, w, h, seq)) = req_rx.recv() {
                        tracing::debug!(seq, "детектор получил запрос");
                        let t0 = Instant::now();
                        // Паника в инференсе/декоде не должна молча убивать
                        // воркер (диагностика 2026-09-01): ловим и отвечаем Err.
                        let lb_w = model.input_w.max(1);
                        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            let (letterboxed, lb) = detector::letterbox_rgb24(&rgb, w, h, lb_w);
                            let outputs = model.infer(&letterboxed).map_err(|e| e.to_string())?;
                            let dets = decoder.decode(&outputs, &dims, &lb, w, h, seq);
                            Ok::<_, String>((dets, t0.elapsed().as_micros() as f32 / 1000.0))
                        }));
                        match res {
                            Ok(Ok((dets, infer_ms))) => {
tracing::debug!(seq, infer_ms, dets = dets.len(), "детекция готова");
                                let _ = resp_tx.send(Ok(DetectResult {
                                    detections: dets,
                                    infer_ms,
                                    frame_seq: seq,
                                }));
                            }
                            Ok(Err(e)) => {
                                tracing::warn!(%e, "infer err");
                                let _ = resp_tx.send(Err(format!("infer: {e}")));
                            }
                            Err(p) => {
                                let msg = format!("panic в детекторе: {p:?}");
                                tracing::error!("{msg}");
                                let _ = resp_tx.send(Err(msg));
                            }
                        }
                    }
                    tracing::info!("детектор-воркер завершён");
                })
                .context("spawn npu-detector")?;
            Ok(Some(handle))
        }
        #[cfg(not(feature = "npu"))]
        {
            tracing::warn!("сборка без фичи npu: детектор-стаб (детекции не выдаются)");
            Ok(None)
        }
    }

    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_arguments)]
    fn process_frame(
        &self,
        cfg: &AppConfig,
        hybrid: &mut HybridTracker,
        stream: Option<&StreamCtx>,
        #[allow(unused_mut)] // mut нужен linux-телу (as_deref_mut)
        mut commander: Option<&mut CommanderCtx>,
        control: Option<&std::sync::Arc<control::ControlLink>>,
        det_req_tx: &std_mpsc::SyncSender<(Vec<u8>, u32, u32, u64)>,
        det_resp_rx: &std_mpsc::Receiver<Result<DetectResult, String>>,
        stats: &mut RunStats,
        mut rgb: Vec<u8>,
        w: u32,
        h: u32,
        seq: u64,
        fps: f32,
        track_ms: &mut f32,
        det_ms: &mut Option<f32>,
        frame_recv: Instant,
        diag: &mut diag::DiagSink,
        last_dets: &mut Vec<Detection>,
    ) -> Result<bool> {
        let mut last_det_conf = 0.0f32;
        // Демо-режим: синтетическая детекция в центре кадра — проверка
        // трекинга/гибрида на реальных кадрах (модель bkb видит только свой
        // целевой класс, которого в лабораторной сцене нет).
        if self.demo_detect && hybrid.wants_detection(seq) && !hybrid.detect_inflight {
            let size = w.min(h) as f32 * 0.25;
            let det = Detection {
                bbox: common::BBox::new(
                    w as f32 / 2.0 - size / 2.0,
                    h as f32 / 2.0 - size / 2.0,
                    size,
                    size,
                ),
                class_id: 0,
                class_name: "demo".into(),
                confidence: 0.9,
                frame_seq: seq,
                detected_at_ms: 0,
            };
            stats.detections_run += 1;
            stats.detect_us_total += 1000;
            *det_ms = Some(1.0);
            let img = Img::new(rgb.clone(), w, h);
            let st = hybrid.on_detection(std::slice::from_ref(&det), &img);
            if st.mode == Mode::DetectAcquire {
                stats.reacquires += 1;
            }
            stats.detections_hits += 1;
        }

        // 0) Команды операторского UI (ADR-016): lock/arm/stop.
        if let Some(ctl) = control {
            while let Some(cmd) = ctl.take_cmd() {
                match cmd {
                    control::UiCmd::Lock { x, y, size } => {
                        let half = (size / 2.0).max(4.0);
                        let bbox = common::BBox::new(x - half, y - half, size, size);
                        let img = Img::new(rgb.clone(), w, h);
                        let st = hybrid.on_manual_roi(bbox, &img);
                        tracing::info!(x, y, size, mode = ?st.mode, "UI: ручной захват цели");
                        stats.reacquires += 1;
                    }
                    control::UiCmd::Arm { on } => {
                        if let Some(c) = commander.as_deref_mut() {
                            c.set_armed(on);
                        }
                        ctl.set_armed(on);
                        tracing::info!(on, "UI: АРМ наведения");
                    }
                    control::UiCmd::Stop => {
                        if let Some(c) = commander.as_deref_mut() {
                            c.emergency_stop();
                        }
                        ctl.set_armed(false);
                        tracing::info!("UI: СТОП наведения");
                    }
                }
            }
        }

        // 1) Отправить кадр на детекцию, если пора и детектор свободен.
        if !self.demo_detect && hybrid.wants_detection(seq) && !hybrid.detect_inflight {
            if det_req_tx.try_send((rgb.clone(), w, h, seq)).is_ok() {
                hybrid.detect_inflight = true;
                stats.detections_run += 1;
            }
        }

        // 2) Забрать результат детекции, если готов.
        if hybrid.detect_inflight {
            if let Ok(resp) = det_resp_rx.try_recv() {
                det_ms.take();
                match resp {
                    Ok(r) => {
                        *det_ms = Some(r.infer_ms);
                        // L2: сырые детекции (до порога) — материал для
                        // офлайн-выбора порога.
                        for d in &r.detections {
                            diag.raw_detection(seq, d.class_id, d.confidence, &d.bbox);
                        }
                        // пер-класс пороги (фаза A): класс без записи — глобальный
                        let dets: Vec<Detection> = r
                            .detections
                            .iter()
                            .filter(|d| {
                                let t = cfg
                                    .detector
                                    .class_thresholds
                                    .get(&d.class_id)
                                    .copied()
                                    .unwrap_or(cfg.detector.conf_threshold);
                                // Вырожденные полосы (h≈2px, conf ровно 0.5) —
                                // артефакт int8-квантования SingleHead-выхода.
                                d.confidence >= t && d.bbox.w >= 8.0 && d.bbox.h >= 8.0
                            })
                            .cloned()
                            .collect();
                        last_det_conf = dets.iter().map(|d| d.confidence).fold(0.0f32, f32::max);
                        *last_dets = dets.clone();
                        for d in &dets {
                            diag.detection(seq, d.class_id, d.confidence, &d.bbox);
                        }
                        let r = DetectResult { detections: dets, infer_ms: r.infer_ms, frame_seq: r.frame_seq };
                        stats.detect_us_total += (r.infer_ms * 1000.0) as u128;
                        if !r.detections.is_empty() {
                            stats.detections_hits += 1;
                        }
                        let img = Img::new(rgb.clone(), w, h);
                        let st = hybrid.on_detection(&r.detections, &img);
                        if st.mode == Mode::DetectAcquire {
                            stats.reacquires += 1;
                        }
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "детектор вернул ошибку");
                        let img = Img::new(rgb.clone(), w, h);
                        hybrid.on_detection(&[], &img);
                    }
                }
            }
        }

        // 3) Трекинг текущего кадра.
        let t0 = Instant::now();
        let img = Img::new(rgb.clone(), w, h);
        let state = hybrid.on_frame(&img);
        let tus = t0.elapsed().as_micros();
        // Полная латентность: получение кадра (до декодирования) → бокс готов.
        let e2e_us = frame_recv.elapsed().as_micros();
        // L4: вибропрофиль — оценка глобального сдвига кадра.
        if let Some((dx, dy)) = hybrid.last_gmc {
            diag.gmc(seq, dx, dy);
        }
        stats.track_us_total += tus;
        *track_ms = tus as f32 / 1000.0;
        stats.frames += 1;
        match state.mode {
            Mode::Tracking => stats.tracking_frames += 1,
            Mode::Lost => stats.lost_frames += 1,
            Mode::DetectAcquire => {}
        }

        // 4) OSD + снапшоты.
        let color = osd::mode_color(state.mode);
        if let Some(b) = state.bbox {
            osd::draw_rect(&mut rgb, w, h, &b, color, 2);
            let (cx, cy) = b.center();
            osd::draw_crosshair(&mut rgb, w, h, cx as i32, cy as i32, color);
        }
        osd::draw_text(&mut rgb, w, h, &format!("FPS-{fps:.0}"), 8, 8, osd::Rgb::Yellow, 2);
        osd::draw_text(
            &mut rgb,
            w,
            h,
            &format!("S-{:.2}", state.score),
            8,
            26,
            color,
            1,
        );
        // Таймстемп локального времени и уверенность последней детекции —
        // для покадрового сопоставления с эталонным видео (фаза A).
        let now = chrono::Local::now();
        osd::draw_text(
            &mut rgb,
            w,
            h,
            &now.format("%H:%M:%S").to_string(),
            w as i32 - 62,
            8,
            osd::Rgb::Yellow,
            1,
        );
        if last_det_conf > 0.0 {
            osd::draw_text(
                &mut rgb,
                w,
                h,
                &format!("D-{last_det_conf:.2}"),
                8,
                36,
                osd::Rgb::Cyan,
                1,
            );
        }
        // Сырые детекции (все, не только сопровождаемая цель) — красным,
        // тонкая рамка: визуальная отладка детектора в стриме (фаза A).
        for d in last_dets.iter() {
            osd::draw_rect(&mut rgb, w, h, &d.bbox, osd::Rgb::Red, 1);
        }
        // Снапшоты — только в diag-режиме (ADR-017), в каталог прогона.
        if diag.enabled() && seq % cfg.output.snapshot_every.max(1) as u64 == 0 {
            if let Ok(j) = encode_jpeg_bytes(&rgb, w, h, 80) {
                diag.snapshot(seq, &j);
            }
        }

        // 4б) Живой стрим: кадр уже с OSD — отдаём энкодер-потоку, только
        // если есть зритель (listen или push), иначе JPEG не кодируем вовсе.
        if let Some(st) = stream {
            if st.wanted() && seq % st.frame_div.max(1) as u64 == 0 {
                st.enc_tx.send((rgb.clone(), w, h));
            }
        }

        // 5) Контур наведения: ошибка (цель − центр) → RC (фаза D).
        if let Some(cmd) = commander.as_deref_mut() {
            let target = state.bbox.map(|b| {
                let (cx, cy) = b.center();
                (cx, cy)
            });
            cmd.tick(state.mode, target, (w, h));
            // L3: журнал тика для офлайн-тюнинга PID.
            if diag.enabled() && cmd.last_logged.is_some() {
                let (err, vel, lead, ch) = cmd.last_logged.unwrap();
                let mode_s = match state.mode {
                    Mode::Tracking => "TRACK",
                    Mode::DetectAcquire => "ACQUIRE",
                    Mode::Lost => "LOST",
                };
                diag.commander_tick(seq, mode_s, err, vel, lead, &ch, cmd.armed);
            }
        }

        // 6) Статус операторскому UI (ADR-016).
        if let Some(ctl) = control {
            let dets_json: Vec<String> = last_dets
                .iter()
                .map(|d| {
                    format!(
                        "[{},{},{},{},{:.2}]",
                        d.bbox.x as i32, d.bbox.y as i32,
                        d.bbox.w as i32, d.bbox.h as i32, d.confidence
                    )
                })
                .collect();
            let box_json = state
                .bbox
                .map(|b| format!("[{},{},{},{}]", b.x as i32, b.y as i32, b.w as i32, b.h as i32))
                .unwrap_or_else(|| "null".into());
            let armed = commander
                .as_deref()
                .map(|c| c.armed)
                .unwrap_or(false);
            let line = format!(
                "{{\"t\":\"status\",\"frame_seq\":{seq},\"mode\":\"{}\",\"score\":{:.3},\"fps\":{:.1},\"e2e_ms\":{:.2},\"box\":{box_json},\"dets\":[{}],\"armed\":{}}}
",
                match state.mode {
                    Mode::Tracking => "TRACK",
                    Mode::DetectAcquire => "ACQUIRE",
                    Mode::Lost => "LOST",
                },
                state.score, fps, e2e_us as f32 / 1000.0,
                dets_json.join(","), armed
            );
            ctl.send_status(line);
        }

        // 7) Телеметрия.
        if diag.enabled() {
            let line = TelemetryLine {
                ts_ms: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0),
                frame_seq: seq,
                mode: match state.mode {
                    Mode::Tracking => "TRACK",
                    Mode::DetectAcquire => "ACQUIRE",
                    Mode::Lost => "LOST",
                },
                x: state.bbox.map(|b| b.x as i32),
                y: state.bbox.map(|b| b.y as i32),
                w: state.bbox.map(|b| b.w as i32),
                h: state.bbox.map(|b| b.h as i32),
                score: (state.score * 1000.0).round() / 1000.0,
                track_ms: (*track_ms * 100.0).round() / 100.0,
                det_ms: det_ms.map(|v| (v * 100.0).round() / 100.0),
                fps: (fps * 10.0).round() / 10.0,
                e2e_ms: (e2e_us as f32 / 1000.0 * 100.0).round() / 100.0,
            };
            let mut s = serde_json_line(&line);
            s.push('\n');
            if diag.enabled() {
                diag.telemetry(&s);
            }
        }
        Ok(true)
    }

    #[allow(clippy::too_many_arguments)]
    fn run_camera(
        &self,
        // Имена без подчёркивания: тело живёт в cfg(target_os="linux")
        // (на Windows параметры формально не читаются — отсюда allow).
        #[allow(unused_variables)]
        cfg: &AppConfig,
        #[allow(unused_variables)]
        hybrid: &mut HybridTracker,
        #[allow(unused_variables)]
        stream: Option<&StreamCtx>,
        #[allow(unused_variables)]
        #[allow(unused_mut)] // mut нужен linux-телу (as_deref_mut)
        mut commander: Option<&mut CommanderCtx>,
        #[allow(unused_variables)]
        control: Option<&std::sync::Arc<control::ControlLink>>,
        #[allow(unused_variables)]
        det_req_tx: &std_mpsc::SyncSender<(Vec<u8>, u32, u32, u64)>,
        #[allow(unused_variables)]
        det_resp_rx: &std_mpsc::Receiver<Result<DetectResult, String>>,
        #[allow(unused_variables)]
        stats: &mut RunStats,
        #[allow(unused_variables)]
        diag: &mut diag::DiagSink,
        #[allow(unused_variables)]
        deadline: Option<Instant>,
    ) -> Result<()> {
        #[cfg(target_os = "linux")]
        {
            use capture::convert::decode_mjpeg_to_rgb;
            use capture::traits::VideoSource;

            let vcfg = capture::VideoSourceConfig::from_common(&cfg.camera);
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()?;

            // Сбойный режим UVC: кадр правильного размера, но содержимое —
            // реплика 3×3. Возникает при быстром re-open и (зафиксировано
            // 2026-09-02) посреди потока. Открытие с валидацией первого
            // кадра вынесено в open_validated_camera — используется и на
            // старте, и для восстановления посреди потока. detile=true —
            // аварийный режим (один тайл на весь кадр).
            let (mut rx, mut src, mut detile) = open_validated_camera(&rt, &vcfg)?;
            tracing::info!(device = %vcfg.device, detile, "захват с камеры запущен");

            let started = Instant::now();
            let mut last_dets: Vec<Detection> = Vec::new();
            let (mut fps, mut track_ms, mut det_ms) = (0f32, 0f32, None);
            let mut fps_counter = FpsCounter::new();
            let mut perf = PerfState::new();
            // Сбойный режим 3×3: периодическая проверка каждые 150 кадров
            // (~5 с); при подозрении — подтверждение двумя соседними кадрами.
            let (mut frames_since_check, mut tiled_streak) = (0u32, 0u32);
            loop {
                if STOP.load(Ordering::SeqCst) {
                    tracing::info!("получен сигнал остановки");
                    break;
                }
                if let Some(d) = deadline {
                    if Instant::now() >= d {
                        break;
                    }
                }
                // L5: время ожидания кадра (захват/джиттер камеры)
                let cap_t0 = Instant::now();
                let frame = match rt.block_on(async {
                    tokio::time::timeout(Duration::from_millis(500), rx.recv()).await
                }) {
                    Ok(Some(f)) => f,
                    Ok(None) => {
                        tracing::warn!("источник кадров закрылся");
                        break;
                    }
                    Err(_) => {
                        // таймаут: кадров нет 500 мс — проверяем флаги и ждём дальше
                        continue;
                    }
                };
                let cap_us = cap_t0.elapsed().as_micros() as u64;
                let (w, h, seq) = (
                    frame.metadata.width,
                    frame.metadata.height,
                    frame.metadata.seq,
                );
                let frame_recv = Instant::now(); // e2e: до декодирования
                let dec_t0 = Instant::now();
                let rgb_frame: Frame = match frame.metadata.format {
                    PixelFormat::Mjpeg => {
                        decode_mjpeg_to_rgb(&frame).context("декодирование MJPEG")?
                    }
                    // PS Eye (ov534): raw Bayer GRBG → RGB24.
                    PixelFormat::BayerGrbg => capture::convert::demosaic_grbg_to_rgb24(&frame)
                        .context("демозаик GRBG")?,
                    _ => frame,
                };
                let dec_us = dec_t0.elapsed().as_micros() as u64;
                fps = fps_counter.tick();
                // Периодический контроль сбойного режима 3×3 (может
                // наступить и посреди потока, зафиксировано на железе).
                // В аварийном режиме detile проверка отключена.
                if !detile {
                    frames_since_check += 1;
                    if tiled_streak > 0 || frames_since_check >= 150 {
                        frames_since_check = 0;
                        let (tiled, mean_diff) =
                            frame_tiled_replication(&rgb_frame.data, w, h);
                        if tiled {
                            tiled_streak += 1;
                            if tiled_streak == 1 {
                                tracing::warn!(mean_diff, "подозрение на сбойный режим 3×3, подтверждаю соседними кадрами");
                            }
                        } else {
                            tiled_streak = 0;
                        }
                        if tiled_streak >= 3 {
                            tracing::warn!(mean_diff, seq, "камера ушла в сбойный режим 3×3 посреди потока — восстановление");
                            rt.block_on(src.stop()).ok();
                            let (nrx, nsrc, ndetile) = open_validated_camera(&rt, &vcfg)?;
                            rx = nrx;
                            src = nsrc;
                            detile = ndetile;
                            tiled_streak = 0;
                            tracing::info!(detile, "камера восстановлена после сбойного режима 3×3");
                            continue; // текущий (битый) кадр в пайплайн не отдаём
                        }
                    }
                }
                // Аварийный режим: один тайл на весь кадр (камера застряла
                // в 3×3).
                let frame_data = if detile {
                    detile3x(&rgb_frame.data, w, h)
                } else {
                    rgb_frame.data
                };
                self.process_frame(
                    cfg, hybrid, stream, commander.as_deref_mut(), control, det_req_tx, det_resp_rx, stats,
                    frame_data, w, h, seq, fps, &mut track_ms, &mut det_ms,
                    frame_recv, diag, &mut last_dets,
                )?;
                // L5: сбор таймингов кадра (track_ms уже посчитан в process_frame)
                perf.push(
                    cap_us,
                    dec_us,
                    (track_ms * 1000.0) as u64,
                    frame_recv.elapsed().as_micros() as u64,
                );
                perf.maybe_flush(stats.frames, diag);
                let _ = started;
            }
            rt.block_on(src.stop())?;
            Ok(())
        }
        #[cfg(not(target_os = "linux"))]
        {
            bail!("захват с камеры поддерживается только на Linux (борту). Используйте --synthetic для локальной отладки.");
        }
    }

    #[allow(clippy::too_many_arguments)]
    /// Оффлайн-прогон записи .mjpg через весь пайплайн (L7). На борту —
    /// с реальным NPU-детектором; на ПК — трекер tract + ручной захват.
    #[allow(clippy::too_many_arguments)]
    fn run_replay(
        &self,
        path: &str,
        rate: f32,
        cfg: &AppConfig,
        hybrid: &mut HybridTracker,
        stream: Option<&StreamCtx>,
        mut commander: Option<&mut CommanderCtx>,
        control: Option<&std::sync::Arc<control::ControlLink>>,
        det_req_tx: &std_mpsc::SyncSender<(Vec<u8>, u32, u32, u64)>,
        det_resp_rx: &std_mpsc::Receiver<Result<DetectResult, String>>,
        stats: &mut RunStats,
        diag: &mut diag::DiagSink,
    ) -> Result<()> {
        let data = std::fs::read(path).with_context(|| format!("чтение {path}"))?;
        let frames = split_mjpeg(&data);
        if frames.is_empty() {
            bail!("в {path} не найдено JPEG-кадров");
        }
        tracing::info!(frames = frames.len(), "replay: запись загружена");
        let (w, h) = (640u32, 480u32);
        let frame_period = Duration::from_secs_f32(1.0 / 30.0);
        let mut last_dets: Vec<Detection> = Vec::new();
        let (mut fps, mut track_ms, mut det_ms) = (0f32, 0f32, None);
        let mut fps_counter = FpsCounter::new();
        let started = Instant::now();
        for (seq, jpeg) in frames.iter().enumerate() {
            if STOP.load(Ordering::SeqCst) {
                break;
            }
            let t0 = Instant::now();
            let rgb = decode_jpeg_rgb(jpeg).context("декодирование кадра записи")?;
            fps = fps_counter.tick();
            self.process_frame(
                cfg, hybrid, stream, commander.as_deref_mut(), control, det_req_tx, det_resp_rx, stats,
                rgb, w, h, seq as u64, fps, &mut track_ms, &mut det_ms,
                t0, diag, &mut last_dets,
            )?;
            // темп: 1.0 = реальные 30 FPS записи; 0 = без пауз
            if rate > 0.0 {
                let target = frame_period.div_f32(rate);
                let spent = t0.elapsed();
                if spent < target {
                    std::thread::sleep(target - spent);
                }
            }
        }
        tracing::info!(
            frames = frames.len(),
            secs = started.elapsed().as_secs_f32(),
            "replay завершён"
        );
        Ok(())
    }

    fn run_synthetic(
        &self,
        cfg: &AppConfig,
        hybrid: &mut HybridTracker,
        stream: Option<&StreamCtx>,
        #[allow(unused_mut)] // mut нужен linux-телу (as_deref_mut)
        mut commander: Option<&mut CommanderCtx>,
        control: Option<&std::sync::Arc<control::ControlLink>>,
        det_req_tx: &std_mpsc::SyncSender<(Vec<u8>, u32, u32, u64)>,
        det_resp_rx: &std_mpsc::Receiver<Result<DetectResult, String>>,
        stats: &mut RunStats,
        diag: &mut diag::DiagSink,
        deadline: Option<Instant>,
    ) -> Result<()> {
        // Синтетический сценарий: квадрат движется по Лиссажу.
        // Детектор заменён точным «оракулом» — отдельный воркер не нужен:
        // эмулируем ответы прямо здесь.
        let (w, h) = (640u32, 480u32);
        let mut seq = 0u64;
        let mut last_dets: Vec<Detection> = Vec::new();
        #[allow(unused_assignments)] // fps перезаписывается счётчиком до чтения
        let (mut fps, mut track_ms, mut det_ms) = (0f32, 0f32, None);
        let mut fps_counter = FpsCounter::new();
        let t0 = Instant::now();
        loop {
            if let Some(d) = deadline {
                if Instant::now() >= d {
                    break;
                }
            }
            let t = t0.elapsed().as_secs_f32();
            let frame = synthetic::synth_frame(w, h, t, cfg.synthetic.shake_px);
            // «Детекция»: раз в N кадров — точный бокс (с шумом как у реальной сети).
            if hybrid.wants_detection(seq) && !hybrid.detect_inflight {
                stats.detections_run += 1;
                let bbox = synthetic::target_bbox(w, h, t, cfg.synthetic.shake_px);
                let dets = vec![Detection {
                    bbox: noisy(bbox, 2.0),
                    class_id: 0,
                    class_name: "target".into(),
                    confidence: 0.88,
                    frame_seq: seq,
                    detected_at_ms: 0,
                }];
                det_ms = Some(28.0); // имитация NPU
                stats.detect_us_total += 28000;
                if !dets.is_empty() {
                    stats.detections_hits += 1;
                }
                let img = Img::new(frame.data.clone(), w, h);
                let st = hybrid.on_detection(&dets, &img);
                if st.mode == Mode::DetectAcquire {
                    stats.reacquires += 1;
                }
            }
            let _ = det_req_tx;
            let _ = det_resp_rx;
            fps = fps_counter.tick();
            let frame_recv = Instant::now();
            self.process_frame(
                cfg, hybrid, stream, commander.as_deref_mut(), control, det_req_tx, det_resp_rx, stats,
                frame.data, w, h, seq, fps, &mut track_ms, &mut det_ms,
                frame_recv, diag, &mut last_dets,
            )?;
            seq += 1;
            std::thread::sleep(Duration::from_millis(33)); // ~30 FPS
        }
        Ok(())
    }
}

/// Сборщик производительности/здоровья (L5): p50/p95 каждые 5 с в perf.jsonl.
struct PerfState {
    started: Instant,
    cap_us: Vec<u64>,
    dec_us: Vec<u64>,
    track_us: Vec<u64>,
    e2e_us: Vec<u64>,
    last_flush: Instant,
}

impl PerfState {
    fn new() -> Self {
        Self {
            started: Instant::now(),
            cap_us: Vec::with_capacity(512),
            dec_us: Vec::with_capacity(512),
            track_us: Vec::with_capacity(512),
            e2e_us: Vec::with_capacity(512),
            last_flush: Instant::now(),
        }
    }

    fn push(&mut self, cap: u64, dec: u64, track: u64, e2e: u64) {
        self.cap_us.push(cap);
        self.dec_us.push(dec);
        self.track_us.push(track);
        self.e2e_us.push(e2e);
    }

    fn maybe_flush(&mut self, frames: u64, diag: &mut diag::DiagSink) {
        if self.last_flush.elapsed() < Duration::from_secs(5) {
            return;
        }
        let p = |v: &mut Vec<u64>| -> (f32, f32) {
            if v.is_empty() {
                return (0.0, 0.0);
            }
            v.sort_unstable();
            let n = v.len();
            let r = (v[n / 2] as f32 / 1000.0, v[(n * 95) / 100] as f32 / 1000.0);
            v.clear();
            r
        };
        let (cap50, cap95) = p(&mut self.cap_us);
        let (dec50, dec95) = p(&mut self.dec_us);
        let (tr50, tr95) = p(&mut self.track_us);
        let (e50, e95) = p(&mut self.e2e_us);
        let rss = std::fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|s| {
                s.lines()
                    .find(|l| l.starts_with("VmRSS:"))
                    .and_then(|l| l.split_whitespace().nth(1).map(|v| v.to_string()))
            })
            .unwrap_or_default();
        let temp = std::fs::read_to_string("/sys/class/thermal/thermal_zone0/temp")
            .ok()
            .and_then(|s| s.trim().parse::<f32>().ok().map(|t| t / 1000.0))
            .unwrap_or(0.0);
        let up = self.started.elapsed().as_secs();
        let line = format!(
            "{{\"t_s\":{up},\"frames\":{frames},\"cap_ms\":[{cap50:.2},{cap95:.2}],\"dec_ms\":[{dec50:.2},{dec95:.2}],\"track_ms\":[{tr50:.2},{tr95:.2}],\"e2e_ms\":[{e50:.2},{e95:.2}],\"rss_kb\":{rss},\"soc_c\":{temp:.1}}}
"
        );
        diag.perf(&line);
        self.last_flush = Instant::now();
    }
}

struct FpsCounter {
    window_start: Instant,
    frames: u32,
    last: f32,
}

impl FpsCounter {
    fn new() -> Self {
        Self {
            window_start: Instant::now(),
            frames: 0,
            last: 0.0,
        }
    }
    fn tick(&mut self) -> f32 {
        self.frames += 1;
        let el = self.window_start.elapsed().as_secs_f32();
        if el >= 0.5 {
            self.last = self.frames as f32 / el;
            self.frames = 0;
            self.window_start = Instant::now();
        }
        self.last
    }
}

/// Поднять gst-конвейер аппаратного H.264: stdin(JPEG) → jpegdec →
/// mpph264enc → matroskamux → файл. Возвращает ребёнка и stdin.
#[cfg(target_os = "linux")]
fn spawn_h264_gst(path: &str) -> Result<(std::process::Child, std::process::ChildStdin)> {
    use std::io::Write as _;
    use std::process::{Command, Stdio};
    let mut child = Command::new("gst-launch-1.0")
        .args([
            "-e",
            "fdsrc",
            "!",
            // JPEG-элементы gst на этой вендорной сборке нерабочи —
            // кормим сырым NV12 через rawvideoparse (проверено 2026-09-03).
            "rawvideoparse",
            "use-sink-caps=false",
            "format=nv12",
            "width=640",
            "height=480",
            "framerate=30/1",
            "!",
            "mpph264enc",
            "bps=2000000",
            "!",
            "h264parse",
            "!",
            "matroskamux",
            "!",
            "filesink",
            &format!("location={path}"),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| "запуск gst-launch (пакет gstreamer1.0-rockchip1)")?;
    let stdin = child.stdin.take().context("stdin gst")?;
    Ok((child, stdin))
}

/// Разбор M-JPEG файла (конкатенация JPEG) на кадры.
fn split_mjpeg(data: &[u8]) -> Vec<&[u8]> {
    const SOI: [u8; 3] = [0xFF, 0xD8, 0xFF];
    const EOI: [u8; 2] = [0xFF, 0xD9];
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(rel) = data[i..].windows(3).position(|w| *w == SOI) {
        let start = i + rel;
        let end = data[start + 3..]
            .windows(2)
            .position(|w| *w == EOI)
            .map(|p| start + 3 + p + 2);
        match end {
            Some(e) if e <= data.len() => {
                out.push(&data[start..e]);
                i = e;
            }
            _ => break,
        }
    }
    out
}

/// JPEG → RGB24 packed.
fn decode_jpeg_rgb(jpeg: &[u8]) -> Result<Vec<u8>> {
    let mut dec = jpeg_decoder::Decoder::new(jpeg);
    let pixels = dec.decode().context("jpeg decode")?;
    let info = dec.info().context("jpeg info")?;
    match info.pixel_format {
        jpeg_decoder::PixelFormat::RGB24 => Ok(pixels),
        jpeg_decoder::PixelFormat::L8 => {
            let mut rgb = Vec::with_capacity(pixels.len() * 3);
            for &p in &pixels {
                rgb.extend_from_slice(&[p, p, p]);
            }
            Ok(rgb)
        }
        _ => bail!("неподдерживаемый формат JPEG: {:?}", info.pixel_format),
    }
}

fn noisy(mut b: common::BBox, amp: f32) -> common::BBox {
    b.x += (b.w * 0.01) * amp * (b.x % 7.0 - 3.0).sin();
    b.y += (b.h * 0.01) * amp * (b.y % 5.0 - 2.0).cos();
    b
}

/// Детектор сбойного режима камеры (быстрый re-open UVC): кадр правильного
/// размера, но содержимое — реплика одной сцены 3×3. У живой сцены соседние
/// девятые кадра различаются заметно сильнее, чем копии. Возврат: (tiled,
/// средняя |разность|). Однородная сцена (стена) не детектируется намеренно.
/// Кадр без текстуры (тёмный прогрев автоэкспозиции): std < 5.
#[cfg(target_os = "linux")]
fn uniform_frame(rgb: &[u8]) -> bool {
    let mut sum = 0u64;
    let mut sum_sq = 0u64;
    let mut n = 0u64;
    for px in rgb.chunks_exact(3).step_by(97) {
        let v = px[0] as u64;
        sum += v;
        sum_sq += v * v;
        n += 1;
    }
    let mean = sum as f64 / n as f64;
    let var = (sum_sq as f64 / n as f64 - mean * mean).max(0.0);
    var.sqrt() < 5.0
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))] // используется только камерой (и тестами)
/// Порог mean_diff между девятыми кадра: сбойная реплика 3×3 ниже, живая
/// сцена выше. Калибровка на живом железе (2026-09-02): реальный сбойный
/// кадр дал 11.6 (тайлы — копии, но с небольшим яркостным смещением на
/// тайл), чистая сцена той же камеры — 85. PS Eye (GRBG, 2026-09-04) дала
/// чистую сцену 34.6 — поэтому одного mean_diff мало: добавлен второй
/// критерий — корреляция девятых (реплика ≈ 0.99, сцена — низкая).
const TILED_MEAN_DIFF_LIMIT: f32 = 30.0;
/// Порог корреляции: у реплики девятые линейно связаны (после вычитания
/// среднего — яркостные смещения тайлов гаснут), у живой сцены — нет.
const TILED_CORR_LIMIT: f32 = 0.85;

#[cfg_attr(not(target_os = "linux"), allow(dead_code))] // используется только гвардом камеры
fn frame_tiled_replication(rgb: &[u8], w: u32, h: u32) -> (bool, f32) {
    let (w, h) = (w as usize, h as usize);
    if w < 24 || h < 24 || rgb.len() < w * h * 3 {
        return (false, 0.0);
    }
    let (tw, th) = (w / 3, h / 3);
    let step = (tw / 40).clamp(1, 8).max((th / 30).clamp(1, 8));
    let mut acc = 0u64;
    let mut diffs = 0u64;
    let mut samples = 0u64;
    let mut sum = 0u64;
    let mut sum_sq = 0u64;
    // Накопители корреляции пар (левый-средний) и (левый-нижний).
    let (mut h_sa, mut h_sb, mut h_saa, mut h_sbb, mut h_sab) = (0i64, 0i64, 0i64, 0i64, 0i64);
    let (mut v_sa, mut v_sb, mut v_saa, mut v_sbb, mut v_sab) = (0i64, 0i64, 0i64, 0i64, 0i64);
    for y in (0..th).step_by(step) {
        for x in (0..tw).step_by(step) {
            let a = (y * w + x) * 3;
            let right = (y * w + tw + x) * 3;
            let below = ((y + th) * w + x) * 3;
            let ra = rgb[a] as u32 as i64;
            let rr = rgb[right] as u32 as i64;
            let rb = rgb[below] as u32 as i64;
            acc += (ra as u32).abs_diff(rr as u32) as u64;
            acc += (ra as u32).abs_diff(rb as u32) as u64;
            diffs += 2;
            h_sa += ra; h_sb += rr; h_saa += ra * ra; h_sbb += rr * rr; h_sab += ra * rr;
            v_sa += ra; v_sb += rb; v_saa += ra * ra; v_sbb += rb * rb; v_sab += ra * rb;
            sum += ra as u64;
            sum_sq += (ra * ra) as u64;
            samples += 1;
        }
    }
    let mean = sum as f64 / samples as f64;
    let var = (sum_sq as f64 / samples as f64 - mean * mean).max(0.0);
    if var.sqrt() < 12.0 {
        return (false, 0.0); // однородная сцена — не различить
    }
    let mean_diff = acc as f32 / diffs as f32;
    if mean_diff >= TILED_MEAN_DIFF_LIMIT {
        return (false, mean_diff);
    }
    // Корреляция Пирсона по сэмплам; вырожденные знаменатели → 0.
    let corr = |sa: i64, sb: i64, saa: i64, sbb: i64, sab: i64| -> f32 {
        let n = samples as f64;
        let num = n * sab as f64 - sa as f64 * sb as f64;
        let da = n * saa as f64 - sa as f64 * sa as f64;
        let db = n * sbb as f64 - sb as f64 * sb as f64;
        if da <= 0.0 || db <= 0.0 {
            return 0.0;
        }
        (num / (da * db).sqrt()) as f32
    };
    let ch = corr(h_sa, h_sb, h_saa, h_sbb, h_sab);
    let cv = corr(v_sa, v_sb, v_saa, v_sbb, v_sab);
    (ch > TILED_CORR_LIMIT && cv > TILED_CORR_LIMIT, mean_diff)
}

/// Аварийный режим «камера застряла в 3×3»: тайлы идентичны, поэтому
/// берём левый верхний тайл и растягиваем на весь кадр (nearest ×3).
/// Оператор получает цельную картинку (с ~3× зумом) вместо 9 копий.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn detile3x(rgb: &[u8], w: u32, h: u32) -> Vec<u8> {
    let (w, h) = (w as usize, h as usize);
    let (tw, th) = (w / 3, h / 3);
    let mut out = vec![0u8; w * h * 3];
    for y in 0..h {
        let sy = y * th / h;
        for x in 0..w {
            let sx = x * tw / w;
            let src = (sy * w + sx) * 3;
            let dst = (y * w + x) * 3;
            out[dst] = rgb[src];
            out[dst + 1] = rgb[src + 1];
            out[dst + 2] = rgb[src + 2];
        }
    }
    out
}

/// Открытие камеры с валидацией первого кадра против сбойного режима 3×3.
/// До 3 попыток с паузой 3 с; перед второй дополнительно USB reset драйвера
/// uvcvideo (usb_camera_reset). Используется на старте и для восстановления
/// посреди потока. Возвращает (rx, src, detile): если сбойный режим не
/// прошёл после 3 попыток — работаем в аварийном режиме detile3x (один тайл
/// на весь кадр), а не падаем в цикл перезапусков. Err — только если камера
/// вообще не отдаёт кадры.
#[cfg(target_os = "linux")]
fn open_validated_camera(
    rt: &tokio::runtime::Runtime,
    vcfg: &capture::VideoSourceConfig,
) -> Result<(
    tokio::sync::mpsc::Receiver<Frame>,
    capture::V4l2DirectSource,
    bool,
)> {
    use capture::convert::decode_mjpeg_to_rgb;
    use capture::traits::VideoSource;

    let mut last_fail: Option<(&str, f32)> = None;
    for attempt in 1..=3u32 {
        if attempt > 1 {
            if attempt == 2 {
                usb_camera_reset();
            }
            std::thread::sleep(Duration::from_secs(3));
        }
        let mut src = capture::V4l2DirectSource::new(
            vcfg.device.clone(),
            vcfg.width,
            vcfg.height,
            vcfg.fps,
        )
        .with_format(vcfg.format);
        let mut rx = match rt.block_on(src.start()) {
            Ok(rx) => rx,
            Err(e) => return Err(e).context("запуск захвата"),
        };
        // Кадр → RGB для проверок (MJPEG-декод или демозаик Bayer).
        let to_rgb = |f: &Frame| -> Option<Frame> {
            match f.metadata.format {
                PixelFormat::Mjpeg => decode_mjpeg_to_rgb(f).ok(),
                PixelFormat::BayerGrbg => capture::convert::demosaic_grbg_to_rgb24(f).ok(),
                _ => Some(f.clone()),
            }
        };
        // Автоэкспозиция прогревается тёмным кадром — гвард «однородной
        // сцены» может пропустить тайлы. До 10 кадров ждём текстуру; если
        // кадры идут, но сцена без текстуры (тёмная комната) — принимаем:
        // тайл-проверка на таком кадре невозможна в принципе.
        let mut first = None;
        let mut last_uniform = None;
        for _ in 0..10 {
            match rt.block_on(async {
                tokio::time::timeout(Duration::from_secs(3), rx.recv()).await
            }) {
                Ok(Some(f)) => {
                    if let Some(rgbf) = to_rgb(&f) {
                        if uniform_frame(&rgbf.data) {
                            tracing::debug!("кадр однородный (прогрев экспозиции) — берём следующий");
                            last_uniform = Some(f);
                            continue;
                        }
                    }
                    first = Some(f);
                    break;
                }
                _ => break,
            }
        }
        let frame = match first.or(last_uniform) {
            Some(f) => f,
            None => {
                tracing::warn!(attempt, "кадр не пришёл за 3 с");
                last_fail = Some(("нет кадра", 0.0));
                rt.block_on(src.stop()).ok();
                continue;
            }
        };
        if let Some(rgbf) = to_rgb(&frame) {
            let (tiled, mean_diff) = frame_tiled_replication(
                &rgbf.data,
                rgbf.metadata.width,
                rgbf.metadata.height,
            );
            tracing::info!(attempt, tiled, mean_diff, "первый кадр проверен");
            if tiled {
                tracing::warn!(
                    attempt,
                    mean_diff,
                    "камера в сбойном режиме (кадр 3×3), пересоздаю источник"
                );
                last_fail = Some(("tiled", mean_diff));
                rt.block_on(src.stop()).ok();
                continue;
            }
        }
        return Ok((rx, src, false));
    }
    // Камера отдаёт кадры, но застряла в 3×3 даже после USB reset —
    // аварийный режим: один тайл на весь кадр (детект сетки отключаем,
    // FOV сужается в 3 раза — громко предупреждаем).
    if last_fail.is_some_and(|(why, _)| why == "tiled") {
        tracing::error!(
            "камера застряла в сбойном режиме 3×3 после всех попыток — \
             включаю аварийный режим detile (один тайл на весь кадр, ~3× зум)"
        );
        let mut src = capture::V4l2DirectSource::new(
            vcfg.device.clone(),
            vcfg.width,
            vcfg.height,
            vcfg.fps,
        )
        .with_format(vcfg.format);
        let rx = rt.block_on(src.start()).context("запуск захвата (detile)")?;
        return Ok((rx, src, true));
    }
    Err(anyhow::anyhow!(
        "камера {:?} не отдаёт кадры, последний сбой: {:?}",
        vcfg.device,
        last_fail
    ))
}

/// USB reset драйвера uvcvideo (unbind/bind всех интерфейсов камеры).
/// Требует root — вызывается через sudo-скрипт без пароля
/// (tools/usb_camera_reset.sh + sudoers NOPASSWD на борту). Ошибки не
/// фатальны: остаётся обычный re-open с паузой.
#[cfg(target_os = "linux")]
fn usb_camera_reset() {
    const SCRIPT: &str = "/home/radxa/synergy/tools/usb_camera_reset.sh";
    match std::process::Command::new("sudo")
        .args(["-n", SCRIPT])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
    {
        Ok(s) if s.success() => tracing::info!("USB reset uvcvideo выполнен"),
        Ok(s) => tracing::warn!(?s, "usb_camera_reset.sh завершился неуспешно"),
        Err(e) => tracing::warn!(%e, "USB reset недоступен (sudo NOPASSWD не настроен?)"),
    }
}

#[cfg(test)]
mod tests {
    use super::{detile3x, frame_tiled_replication};

    #[test]
    fn tiled_replication_detected() {
        let (w, h) = (640u32, 480u32);
        // Живая сцена в левом верхнем тайле, реплицированная 3×3.
        let mut tile = vec![0u8; (w / 3 * h / 3 * 3) as usize];
        for (i, p) in tile.iter_mut().enumerate() {
            let (x, y) = (i / 3 % (w as usize / 3), i / 3 / (w as usize / 3));
            *p = ((x * 7 + y * 13) % 251) as u8;
        }
        let mut rgb = vec![0u8; (w * h * 3) as usize];
        for ty in 0..3 {
            for tx in 0..3 {
                for y in 0..h as usize / 3 {
                    for x in 0..w as usize / 3 {
                        let src = (y * (w as usize / 3) + x) * 3;
                        let dst = ((ty * h as usize / 3 + y) * w as usize
                            + tx * w as usize / 3
                            + x)
                            * 3;
                        rgb[dst..dst + 3].copy_from_slice(&tile[src..src + 3]);
                    }
                }
            }
        }
        let (tiled, d) = frame_tiled_replication(&rgb, w, h);
        assert!(tiled, "реплика 3×3 не распознана, mean_diff={d}");
        // Реальный сбойный кадр с железа: тайлы — копии, но с небольшим
        // яркостным смещением на тайл (замер: mean_diff ≈ 11.6).
        let mut rgbo = rgb.clone();
        for ty in 0..3usize {
            for tx in 0..3usize {
                let off = (ty * 9 + tx * 6) as u8;
                for y in 0..h as usize / 3 {
                    for x in 0..w as usize / 3 {
                        let i = ((ty * h as usize / 3 + y) * w as usize
                            + tx * w as usize / 3
                            + x)
                            * 3;
                        for c in 0..3 {
                            rgbo[i + c] = rgbo[i + c].wrapping_add(off);
                        }
                    }
                }
            }
        }
        let (tiled4, d4) = frame_tiled_replication(&rgbo, w, h);
        assert!(tiled4, "реплика с яркостным смещением не распознана, mean_diff={d4}");
        // Различающаяся сцена (градиент по всему кадру) — не «тайл».
        let mut grad = vec![0u8; (w * h * 3) as usize];
        for i in 0..(w * h) as usize {
            grad[i * 3] = (i / (w as usize) % 256) as u8;
            grad[i * 3 + 1] = (i % (w as usize) % 256) as u8;
        }
        let (tiled2, _) = frame_tiled_replication(&grad, w, h);
        assert!(!tiled2, "градиент ошибочно признан репликой");
        // Однородная стена — детектор сознательно молчит.
        let wall = vec![128u8; (w * h * 3) as usize];
        let (tiled3, _) = frame_tiled_replication(&wall, w, h);
        assert!(!tiled3);
        // Гладкая, но НЕ реплицированная сцена (PS Eye): девятые похожи по
        // яркости (mean_diff < 30), но структура не коррелирует — не «тайл».
        let (tw, th) = (w as usize / 3, h as usize / 3);
        let mut soft = vec![0u8; (w * h * 3) as usize];
        for ty in 0..3usize {
            for tx in 0..3usize {
                // у каждого тайла своя фаза «пятен» → низкая корреляция
                let (py, px) = (ty as f32 * 2.1, tx as f32 * 1.7);
                for y in 0..th {
                    for x in 0..tw {
                        let v = 128.0
                            + 22.0
                                * ((x as f32 / 9.0 + px).sin()
                                    * ((y as f32 / 7.0 + py).cos()));
                        let i = ((ty * th + y) * w as usize + tx * tw + x) * 3;
                        let v8 = v.clamp(0.0, 255.0) as u8;
                        soft[i] = v8;
                        soft[i + 1] = v8;
                        soft[i + 2] = v8;
                    }
                }
            }
        }
        let (tiled5, d5) = frame_tiled_replication(&soft, w, h);
        assert!(!tiled5, "гладкая сцена ошибочно признана репликой, mean_diff={d5}");
    }

    #[test]
    fn detile_picks_single_tile() {
        // Кадр 6×3: левый верхний тайл 2×1 с уникальными значениями,
        // реплицированный 3×3. После detile весь кадр = левый верхний тайл.
        let (w, h) = (6u32, 3u32);
        let (tw, th) = (2usize, 1usize);
        let mut rgb = vec![0u8; (w * h * 3) as usize];
        for ty in 0..3 {
            for tx in 0..3 {
                for y in 0..th {
                    for x in 0..tw {
                        let v = ((y * tw + x) * 30) as u8;
                        let dst = ((ty * th + y) * w as usize + tx * tw + x) * 3;
                        rgb[dst..dst + 3].fill(v);
                    }
                }
            }
        }
        let out = detile3x(&rgb, w, h);
        // Каждый пиксель результата = пиксель левого верхнего тайла.
        for y in 0..h as usize {
            for x in 0..w as usize {
                let src = (y * th / h as usize * tw + x * tw / w as usize) * 3;
                let dst = (y * w as usize + x) * 3;
                assert_eq!(out[dst], rgb[src], "({x},{y})");
            }
        }
    }
}

/// Мини-сериализатор (не тянем serde_json ради одной строки в кадр).
fn serde_json_line(line: &TelemetryLine) -> String {
    let opt = |v: Option<i32>| match v {
        Some(x) => x.to_string(),
        None => "null".into(),
    };
    let optf = |v: Option<f32>| match v {
        Some(x) => x.to_string(),
        None => "null".into(),
    };
    format!(
        "{{\"ts_ms\":{},\"frame_seq\":{},\"mode\":\"{}\",\"x\":{},\"y\":{},\"w\":{},\"h\":{},\"score\":{},\"track_ms\":{},\"det_ms\":{},\"fps\":{},\"e2e_ms\":{}}}",
        line.ts_ms,
        line.frame_seq,
        line.mode,
        opt(line.x),
        opt(line.y),
        opt(line.w),
        opt(line.h),
        line.score,
        line.track_ms,
        optf(line.det_ms),
        line.fps,
        line.e2e_ms
    )
}

/// RGB24 → NV12 (Y плоскость + чересстрочная UV), для аппаратного H.264.
fn rgb24_to_nv12(rgb: &[u8], w: u32, h: u32) -> Vec<u8> {
    let (w, h) = (w as usize, h as usize);
    let mut out = vec![0u8; w * h * 3 / 2];
    let (y_plane, uv_plane) = out.split_at_mut(w * h);
    for yy in 0..h {
        for xx in 0..w {
            let i = (yy * w + xx) * 3;
            let (r, g, b) = (rgb[i] as i32, rgb[i + 1] as i32, rgb[i + 2] as i32);
            y_plane[yy * w + xx] = ((77 * r + 150 * g + 29 * b) >> 8) as u8;
        }
    }
    for yy in 0..h / 2 {
        for xx in 0..w / 2 {
            let base = (2 * yy * w + 2 * xx) * 3;
            let (mut sr, mut sg, mut sb) = (0i32, 0i32, 0i32);
            for dy in 0..2 {
                for dx in 0..2 {
                    let i = base + (dy * w + dx) * 3;
                    sr += rgb[i] as i32;
                    sg += rgb[i + 1] as i32;
                    sb += rgb[i + 2] as i32;
                }
            }
            let (r, g, b) = (sr >> 2, sg >> 2, sb >> 2);
            let uv = (yy * (w / 2) + xx) * 2;
            uv_plane[uv] = (((-43 * r - 85 * g + 128 * b) >> 8) + 128) as u8;
            uv_plane[uv + 1] = (((128 * r - 107 * g - 21 * b) >> 8) + 128) as u8;
        }
    }
    out
}

/// RGB24 → JPEG в память (планарная укладка для jpeg-encoder).
fn encode_jpeg_bytes(rgb: &[u8], w: u32, h: u32, quality: u8) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(rgb.len() / 6);
    {
        let encoder = jpeg_encoder::Encoder::new(&mut out, quality);
        // ВАЖНО: ColorType::Rgb ждёт ИНТЕРЛИВЕД (r,g,b на пиксель).
        // Исторический баг (2026-09-02..04, ADR-018): сюда передавали
        // планарный буфер [RR..GG..BB] — энкодер читал его как построчный
        // и получалась «сетка 3×3» на ЛЮБОЙ камере; камеры были невиновны.
        encoder.encode(rgb, w as u16, h as u16, jpeg_encoder::ColorType::Rgb)?;
    }
    Ok(out)
}

fn save_jpeg(rgb: &[u8], w: u32, h: u32, path: &str) -> Result<()> {
    let bytes = encode_jpeg_bytes(rgb, w, h, 80)?;
    std::fs::write(path, bytes)?;
    Ok(())
}

/// Диагностика tract↔rknn (фаза C): одинаковые кропы, косинус выходов.
/// Проверяет и перестановку layout (RKNN внутренне NHWC — плоский порядок
/// выходов может отличаться от NCHW-порядка tract).
#[cfg(feature = "npu")]
fn diag_nets() -> Result<()> {
    use nano_track::imgops::{get_subwindow, Img};
    use nano_track::{backend_rknn::RknnNets, backend_tract::TractNets, TrackerNets};

    let tract = TractNets::load(
        "models/nanotrack_backbone_127.onnx",
        "models/nanotrack_backbone_sim.onnx",
        "models/nanotrack_head_sim.onnx",
        false,
    )?;
    let rknn = RknnNets::load(
        "models/nanotrack_backbone_127.rknn",
        "models/nanotrack_backbone_255.rknn",
        "models/nanotrack_head.rknn",
        false,
    )?;
    let mut tract = tract;
    let mut rknn = rknn;

    let cos = |a: &[f32], b: &[f32]| -> f32 {
        let d: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
        let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
        d / (na * nb + 1e-9)
    };
    // NCHW-плоский [C,H,W] → NHWC-плоский [H,W,C].
    let to_nhwc = |src: &[f32], c: usize, h: usize, w: usize| -> Vec<f32> {
        let mut dst = vec![0f32; src.len()];
        for y in 0..h {
            for x in 0..w {
                for ch in 0..c {
                    dst[(y * w + x) * c + ch] = src[ch * h * w + y * w + x];
                }
            }
        }
        dst
    };
    // NHWC-плоский [H,W,C] → NCHW-плоский [C,H,W] (и наоборот — та же перестановка).
    let permute = |src: &[f32], c: usize, h: usize, w: usize| -> Vec<f32> {
        // src: NHWC (h*w*c), dst: NCHW
        let mut dst = vec![0f32; src.len()];
        for y in 0..h {
            for x in 0..w {
                for ch in 0..c {
                    dst[ch * h * w + y * w + x] = src[(y * w + x) * c + ch];
                }
            }
        }
        dst
    };

    for t in [0.0f32, 0.7, 1.9, 3.3] {
        let frame = synthetic::synth_frame(640, 480, t, 0.0);
        let img = Img::new(frame.data, 640, 480);
        let (cx, cy) = synthetic::target_position(640, 480, t, 0.0);
        let crop127 = get_subwindow(&img, cx, cy, 180, 127);
        let crop255 = get_subwindow(&img, cx, cy, 360, 255);

        let zf_t = tract.run_backbone_z(&crop127)?;
        let zf_r = rknn.run_backbone_z(&crop127)?;
        let xf_t = tract.run_backbone_x(&crop255)?;
        let xf_r = rknn.run_backbone_x(&crop255)?;

        // zf: 48×8×8, xf: 48×16×16
        let (cz, hz, wz) = (48usize, 8, 8);
        let (cx_, hx, wx) = (48usize, 16, 16);
        println!("t={t}:");
        println!(
            "  zf: cos(raw)={:.4} cos(perm)={:.4} | диапазон tract [{:.2},{:.2}] rknn [{:.2},{:.2}]",
            cos(&zf_t, &zf_r),
            cos(&zf_t, &permute(&zf_r, cz, hz, wz)),
            zf_t.iter().cloned().fold(f32::MAX, f32::min),
            zf_t.iter().cloned().fold(f32::MIN, f32::max),
            zf_r.iter().cloned().fold(f32::MAX, f32::min),
            zf_r.iter().cloned().fold(f32::MIN, f32::max),
        );
        println!(
            "  xf: cos(raw)={:.4} cos(perm)={:.4}",
            cos(&xf_t, &xf_r),
            cos(&xf_t, &permute(&xf_r, cx_, hx, wx)),
        );

        // Голова на ОДИНАКОВЫХ входах (tract-эталон) — изолируем голову.
        let (cls_t, bb_t) = tract.run_head(&zf_t, &xf_t)?;
        let (cls_r, bb_r) = rknn.run_head(&zf_t, &xf_t)?;
        println!(
            "  head(nchw): cls cos={:.4} bbox cos={:.4}",
            cos(&cls_t, &cls_r),
            cos(&bb_t, &bb_r),
        );
        // Вариант: входы головы в NHWC-плоском порядке (гипотеза о драйвере).
        let zf_nhwc = to_nhwc(&zf_t, 48, 8, 8);
        let xf_nhwc = to_nhwc(&xf_t, 48, 16, 16);
        let outs = rknn
            .head_probe(&zf_nhwc, &xf_nhwc)
            .context("head probe")?;
        println!(
            "  head(nhwc): cls cos={:.4} bbox cos={:.4}",
            cos(&cls_t, &outs.0),
            cos(&bb_t, &outs.1),
        );
    }
    Ok(())
}

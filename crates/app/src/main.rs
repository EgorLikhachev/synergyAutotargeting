//! synergyAutotargeting — гибридный трекинг (вариант C):
//! детекция YOLOv8 на NPU раз в N кадров + NanoTrack на CPU каждый кадр.
//! Железо: Radxa ROCK 5A (RK3588S) + USB-камера Arducam.

mod config;
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
        libc::signal(libc::SIGINT, on_signal as libc::sighandler_t);
        libc::signal(libc::SIGTERM, on_signal as libc::sighandler_t);
    }
}

use anyhow::{bail, Context, Result};
use clap::Parser;
use common::{Detection, Frame, PixelFormat};
use nano_track::imgops::Img;
use pipeline::{HybridConfig, HybridTracker, Mode, TargetState};
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
    std::fs::create_dir_all(&cfg.output.dir).ok();

    let run = Runner::new(cfg, args.synthetic, args.demo_detect)?;
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
}

/// Живой стрим OSD: сервер + канал к потоку-энкодеру JPEG.
struct StreamCtx {
    server: streaming::MjpegServer,
    enc_tx: streaming::LatestSender<(Vec<u8>, u32, u32)>,
    frame_div: u32,
}

impl Runner {
    fn new(cfg: AppConfig, synthetic: bool, demo_detect: bool) -> Result<Self> {
        Ok(Self {
            cfg,
            synthetic,
            demo_detect,
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

        // === Трекер (NanoTrack, tract) ===
        let nano = nano_track::NanoTracker::new(
            &cfg.tracker.backbone_path,
            &cfg.tracker.backbone_search_path,
            &cfg.tracker.head_path,
            cfg.tracker.swap_rb,
        )
        .context("загрузка моделей NanoTrack")?;
        let hybrid_cfg = HybridConfig {
            detect_every_n: cfg.pipeline.detect_every_n,
            iou_confirm: cfg.pipeline.iou_confirm,
            min_track_score: cfg.tracker.min_track_score,
            lost_patience: cfg.pipeline.lost_patience,
            min_detect_conf: cfg.pipeline.min_detect_conf,
            priority_classes: cfg.pipeline.priority_classes.clone(),
            use_stabilizer: cfg.pipeline.use_stabilizer,
        };
        let mut hybrid = HybridTracker::new(nano, hybrid_cfg);

        // === Стрим OSD (MJPEG-over-HTTP, ADR-009) ===
        let stream_ctx = if cfg.stream.enabled {
            let server = streaming::MjpegServer::start(&cfg.stream.bind)
                .with_context(|| format!("bind {}", cfg.stream.bind))?;
            tracing::info!(
                port = server.port(),
                "стрим MJPEG: http://<ip>:{}/ (браузер/VLC)", server.port()
            );
            let (enc_tx, enc_rx) = streaming::latest_channel::<(Vec<u8>, u32, u32)>();
            let quality = cfg.stream.quality;
            let srv = server.clone();
            // Кодирование — вне бюджетного потока кадра (§4 HARDWARE_TEST_RESULTS).
            let _enc = std::thread::Builder::new()
                .name("mjpeg-encoder".into())
                .spawn(move || {
                    while let Some((rgb, w, h)) = enc_rx.recv() {
                        match encode_jpeg_bytes(&rgb, w, h, quality) {
                            Ok(j) => srv.push_jpeg(j),
                            Err(e) => tracing::warn!(error = %e, "кадр стрима не закодирован"),
                        }
                    }
                })
                .context("spawn mjpeg-encoder")?;
            Some(StreamCtx { server, enc_tx, frame_div: cfg.stream.frame_div })
        } else {
            None
        };

        // === Детектор ===
        let (det_req_tx, det_req_rx) = std_mpsc::sync_channel::<(Vec<u8>, u32, u32, u64)>(1);
        let (det_resp_tx, det_resp_rx) = std_mpsc::sync_channel::<Result<DetectResult, String>>(1);
        let det_handle = self.spawn_detector(&cfg, det_req_rx, det_resp_tx)?;

        // === Источник кадров ===
        let tel_path = format!("{}/telemetry.jsonl", cfg.output.dir);
        let mut telemetry = std::fs::File::create(&tel_path).context("создание telemetry.jsonl")?;

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

        let result = if self.synthetic {
            self.run_synthetic(&cfg, &mut hybrid, stream_ctx.as_ref(), &det_req_tx, &det_resp_rx, &mut stats, &mut telemetry, deadline)
        } else {
            self.run_camera(&cfg, &mut hybrid, stream_ctx.as_ref(), &det_req_tx, &det_resp_rx, &mut stats, &mut telemetry, deadline)
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
        if let Some(st) = &stream_ctx {
            println!(
                "стрим: {} кадров отдано зрителям",
                st.server.served_frames()
            );
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
            let conf = cfg.detector.conf_threshold;
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
    fn process_frame(
        &self,
        cfg: &AppConfig,
        hybrid: &mut HybridTracker,
        stream: Option<&StreamCtx>,
        det_req_tx: &std_mpsc::SyncSender<(Vec<u8>, u32, u32, u64)>,
        det_resp_rx: &std_mpsc::Receiver<Result<DetectResult, String>>,
        stats: &mut RunStats,
        telemetry: &mut std::fs::File,
        mut rgb: Vec<u8>,
        w: u32,
        h: u32,
        seq: u64,
        fps: f32,
        track_ms: &mut f32,
        det_ms: &mut Option<f32>,
    ) -> Result<bool> {
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
        if seq % cfg.output.snapshot_every.max(1) as u64 == 0 {
            let path = format!("{}/frame_{seq:06}.jpg", cfg.output.dir);
            if let Err(e) = save_jpeg(&rgb, w, h, &path) {
                tracing::warn!(error = %e, path = %path, "снапшот не сохранён");
            }
        }

        // 4б) Живой стрим: кадр уже с OSD — отдаём энкодер-потоку, только
        // если есть зрители (иначе JPEG-кодирование не тратит CPU вовсе).
        if let Some(st) = stream {
            if st.server.clients() > 0 && seq % st.frame_div.max(1) as u64 == 0 {
                st.enc_tx.send((rgb.clone(), w, h));
            }
        }

        // 5) Телеметрия.
        if cfg.output.telemetry {
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
            };
            let mut s = serde_json_line(&line);
            s.push('\n');
            telemetry.write_all(s.as_bytes()).ok();
        }
        Ok(true)
    }

    fn run_camera(
        &self,
        cfg: &AppConfig,
        hybrid: &mut HybridTracker,
        stream: Option<&StreamCtx>,
        det_req_tx: &std_mpsc::SyncSender<(Vec<u8>, u32, u32, u64)>,
        det_resp_rx: &std_mpsc::Receiver<Result<DetectResult, String>>,
        stats: &mut RunStats,
        telemetry: &mut std::fs::File,
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

            // Быстрый re-open камеры (pkill и мгновенный перезапуск) иногда
            // оставляет UVC в сбойном режиме: кадр правильного размера, но
            // содержимое — реплика 3×3 (замечено на железе 2026-09-02).
            // Валидируем первый кадр и пересоздаём источник с паузой.
            let (mut rx, mut src) = 'valid: {
                let mut fail: Option<(&str, f32)> = None;
                for attempt in 1..=3u32 {
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
                    let first = rt.block_on(async {
                        tokio::time::timeout(Duration::from_secs(3), rx.recv()).await
                    });
                    let Ok(Some(frame)) = first else {
                        tracing::warn!(attempt, "первый кадр не пришёл за 3 с");
                        fail = Some(("нет кадра", 0.0));
                        rt.block_on(src.stop()).ok();
                        std::thread::sleep(Duration::from_secs(3));
                        continue;
                    };
                    if frame.metadata.format == PixelFormat::Mjpeg {
                        if let Ok(rgbf) = decode_mjpeg_to_rgb(&frame) {
                            let tiled = frame_tiled_replication(
                                &rgbf.data,
                                rgbf.metadata.width,
                                rgbf.metadata.height,
                            );
                            tracing::info!(
                                attempt,
                                tiled = tiled.0,
                                mean_diff = tiled.1,
                                "первый кадр проверен"
                            );
                            if tiled.0 {
                                tracing::warn!(
                                    attempt,
                                    mean_diff = tiled.1,
                                    "камера в сбойном режиме (кадр 3×3), пересоздаю источник"
                                );
                                fail = Some(("tiled", tiled.1));
                                rt.block_on(src.stop()).ok();
                                std::thread::sleep(Duration::from_secs(3));
                                continue;
                            }
                        }
                    }
                    break 'valid (rx, src);
                }
                // три неудачи — работаем с тем, что есть (пустая стена
                // тоже даёт похожие тайлы), но громко предупреждаем.
                tracing::error!(fail = ?fail, "валидация первого кадра не прошла 3 раза, продолжаю");
                let mut src = capture::V4l2DirectSource::new(
                    vcfg.device.clone(),
                    vcfg.width,
                    vcfg.height,
                    vcfg.fps,
                )
                .with_format(vcfg.format);
                let rx = rt
                    .block_on(src.start())
                    .context("запуск захвата (после 3 попыток)")?;
                (rx, src)
            };
            tracing::info!(device = %vcfg.device, "захват с камеры запущен");

            let started = Instant::now();
            let (mut fps, mut track_ms, mut det_ms) = (0f32, 0f32, None);
            let mut fps_counter = FpsCounter::new();
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
                let (w, h, seq) = (
                    frame.metadata.width,
                    frame.metadata.height,
                    frame.metadata.seq,
                );
                let rgb_frame: Frame = if frame.metadata.format == PixelFormat::Mjpeg {
                    decode_mjpeg_to_rgb(&frame).context("декодирование MJPEG")?
                } else {
                    frame
                };
                fps = fps_counter.tick();
                self.process_frame(
                    cfg, hybrid, stream, det_req_tx, det_resp_rx, stats, telemetry,
                    rgb_frame.data, w, h, seq, fps, &mut track_ms, &mut det_ms,
                )?;
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

    fn run_synthetic(
        &self,
        cfg: &AppConfig,
        hybrid: &mut HybridTracker,
        stream: Option<&StreamCtx>,
        det_req_tx: &std_mpsc::SyncSender<(Vec<u8>, u32, u32, u64)>,
        det_resp_rx: &std_mpsc::Receiver<Result<DetectResult, String>>,
        stats: &mut RunStats,
        telemetry: &mut std::fs::File,
        deadline: Option<Instant>,
    ) -> Result<()> {
        // Синтетический сценарий: квадрат движется по Лиссажу.
        // Детектор заменён точным «оракулом» — отдельный воркер не нужен:
        // эмулируем ответы прямо здесь.
        let (w, h) = (640u32, 480u32);
        let mut seq = 0u64;
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
            let frame = synthetic::synth_frame(w, h, t);
            // «Детекция»: раз в N кадров — точный бокс (с шумом как у реальной сети).
            if hybrid.wants_detection(seq) && !hybrid.detect_inflight {
                stats.detections_run += 1;
                let bbox = synthetic::target_bbox(w, h, t);
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
            self.process_frame(
                cfg, hybrid, stream, det_req_tx, det_resp_rx, stats, telemetry,
                frame.data, w, h, seq, fps, &mut track_ms, &mut det_ms,
            )?;
            seq += 1;
            std::thread::sleep(Duration::from_millis(33)); // ~30 FPS
        }
        Ok(())
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

fn noisy(mut b: common::BBox, amp: f32) -> common::BBox {
    b.x += (b.w * 0.01) * amp * (b.x % 7.0 - 3.0).sin();
    b.y += (b.h * 0.01) * amp * (b.y % 5.0 - 2.0).cos();
    b
}

/// Детектор сбойного режима камеры (быстрый re-open UVC): кадр правильного
/// размера, но содержимое — реплика одной сцены 3×3. У живой сцены соседние
/// девятые кадра различаются заметно сильнее, чем копии. Возврат: (tiled,
/// средняя |разность|). Однородная сцена (стена) не детектируется намеренно.
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
    for y in (0..th).step_by(step) {
        for x in (0..tw).step_by(step) {
            let a = (y * w + x) * 3;
            let right = (y * w + tw + x) * 3;
            let below = ((y + th) * w + x) * 3;
            let ra = rgb[a] as u32;
            acc += (ra).abs_diff(rgb[right] as u32) as u64;
            acc += (ra).abs_diff(rgb[below] as u32) as u64;
            diffs += 2;
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
    (mean_diff < 6.0, mean_diff)
}

#[cfg(test)]
mod tests {
    use super::frame_tiled_replication;

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
        "{{\"ts_ms\":{},\"frame_seq\":{},\"mode\":\"{}\",\"x\":{},\"y\":{},\"w\":{},\"h\":{},\"score\":{},\"track_ms\":{},\"det_ms\":{},\"fps\":{}}}",
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
        line.fps
    )
}

/// RGB24 → JPEG в память (планарная укладка для jpeg-encoder).
fn encode_jpeg_bytes(rgb: &[u8], w: u32, h: u32, quality: u8) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(rgb.len() / 6);
    {
        let mut encoder = jpeg_encoder::Encoder::new(&mut out, quality);
        let mut planar = Vec::with_capacity(rgb.len());
        let mut r = Vec::with_capacity(rgb.len() / 3);
        let mut g = Vec::with_capacity(rgb.len() / 3);
        let mut b = Vec::with_capacity(rgb.len() / 3);
        for px in rgb.chunks_exact(3) {
            r.push(px[0]);
            g.push(px[1]);
            b.push(px[2]);
        }
        planar.extend_from_slice(&r);
        planar.extend_from_slice(&g);
        planar.extend_from_slice(&b);
        encoder.encode(
            &planar,
            w as u16,
            h as u16,
            jpeg_encoder::ColorType::Rgb,
        )?;
    }
    Ok(out)
}

fn save_jpeg(rgb: &[u8], w: u32, h: u32, path: &str) -> Result<()> {
    let bytes = encode_jpeg_bytes(rgb, w, h, 80)?;
    std::fs::write(path, bytes)?;
    Ok(())
}

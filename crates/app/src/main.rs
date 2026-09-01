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
    std::fs::create_dir_all(&cfg.output.dir).ok();

    let run = Runner::new(cfg, args.synthetic)?;
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
}

impl Runner {
    fn new(cfg: AppConfig, synthetic: bool) -> Result<Self> {
        Ok(Self { cfg, synthetic })
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
            self.run_synthetic(&cfg, &mut hybrid, &det_req_tx, &det_resp_rx, &mut stats, &mut telemetry, deadline)
        } else {
            self.run_camera(&cfg, &mut hybrid, &det_req_tx, &det_resp_rx, &mut stats, &mut telemetry, deadline)
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
        let _ = det_req_tx; // закрыть канал — воркер завершится
        if let Some(h) = det_handle {
            let _ = h.join();
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
                        eprintln!("[DET] got request seq={seq} len={}", rgb.len());
                        let t0 = Instant::now();
                        let (letterboxed, lb) =
                            detector::letterbox_rgb24(&rgb, w, h, model.input_w.max(1));
                        eprintln!("[DET] letterbox done, tensor {}x{}", model.input_w, model.input_h);
                        match model.infer(&letterboxed) {
                            Ok(outputs) => {
                                let infer_us = t0.elapsed().as_micros();
                                eprintln!("[DET] infer OK за {} mks, выходов={}", infer_us, outputs.len());
                                let dets = decoder.decode(
                                    &outputs,
                                    &dims,
                                    &lb,
                                    w,
                                    h,
                                    seq,
                                );
                                let _ = resp_tx.send(Ok(DetectResult {
                                    detections: dets,
                                    infer_ms: infer_us as f32 / 1000.0,
                                    frame_seq: seq,
                                }));
                            }
                            Err(e) => {
                                eprintln!("[DET] infer ERR: {e}");
                                let _ = resp_tx.send(Err(format!("infer: {e}")));
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
        // 1) Отправить кадр на детекцию, если пора и детектор свободен.
        if seq % 30 == 0 {
            eprintln!(
                "[MAIN] seq={seq} inflight={} wants={}",
                hybrid.detect_inflight,
                hybrid.wants_detection(seq)
            );
        }
        if hybrid.wants_detection(seq) && !hybrid.detect_inflight {
            if det_req_tx.try_send((rgb.clone(), w, h, seq)).is_ok() {
                hybrid.detect_inflight = true;
                stats.detections_run += 1;
            } else {
                eprintln!("[MAIN] try_send FAILED seq={seq}");
            }
        }

        // 2) Забрать результат детекции, если готов.
        if hybrid.detect_inflight {
            if let Ok(resp) = det_resp_rx.try_recv() {
                eprintln!("[MAIN] got detection response at seq={seq}");
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
            let mut src = capture::V4l2DirectSource::new(
                vcfg.device.clone(),
                vcfg.width,
                vcfg.height,
                vcfg.fps,
            )
            .with_format(vcfg.format);
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()?;
            let mut rx = rt.block_on(src.start())?;
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
                let frame = match rx.blocking_recv() {
                    Some(f) => f,
                    None => {
                        tracing::warn!("источник кадров закрылся");
                        break;
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
                    cfg, hybrid, det_req_tx, det_resp_rx, stats, telemetry,
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
                cfg, hybrid, det_req_tx, det_resp_rx, stats, telemetry,
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

fn save_jpeg(rgb: &[u8], w: u32, h: u32, path: &str) -> Result<()> {
    let mut out = std::fs::File::create(path)?;
    let mut encoder = jpeg_encoder::Encoder::new(&mut out, 80);
    let mut buf = Vec::with_capacity(rgb.len() / 3);
    // RGB24 → планарный для jpeg-encoder
    let mut r = Vec::with_capacity(rgb.len() / 3);
    let mut g = Vec::with_capacity(rgb.len() / 3);
    let mut b = Vec::with_capacity(rgb.len() / 3);
    for px in rgb.chunks_exact(3) {
        r.push(px[0]);
        g.push(px[1]);
        b.push(px[2]);
    }
    buf.extend_from_slice(&r);
    buf.extend_from_slice(&g);
    buf.extend_from_slice(&b);
    encoder.encode(
        &buf,
        w as u16,
        h as u16,
        jpeg_encoder::ColorType::Rgb,
    )?;
    Ok(())
}

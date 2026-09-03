//! Приёмник диагностики прогона (ADR-017). Два режима:
//!
//! - **battle** (по умолчанию): ничего не пишется на диск — максимальная
//!   чистота боевого режима. Работают только явные действия оператора
//!   (--record/--record-h264) и стрим/UI.
//! - **diag** (`--diag` или `[logging] mode="diag"`): каталог прогона
//!   `data/runs/ГГГГ-ММ-ДД_ЧЧ-ММ-СС/` со всеми журналами:
//!   session.json, telemetry.jsonl, detections.jsonl (+raw),
//!   commander.jsonl, gmc.jsonl, perf.jsonl, snaps/.

use std::fs::{self, File};
use std::io::Write as _;
use std::path::PathBuf;

use crate::config::AppConfig;

pub struct DiagSink {
    dir: Option<PathBuf>,
    telemetry: Option<File>,
    detections: Option<File>,
    raw_detections: Option<File>,
    commander: Option<File>,
    gmc: Option<File>,
    perf: Option<File>,
}

impl DiagSink {
    /// Боевой режим: все каналы закрыты, записи нет.
    pub fn disabled() -> Self {
        Self {
            dir: None,
            telemetry: None,
            detections: None,
            raw_detections: None,
            commander: None,
            gmc: None,
            perf: None,
        }
    }

    /// Диагностический режим: создать каталог прогона и открыть журналы.
    pub fn open(cfg: &AppConfig) -> std::io::Result<Self> {
        let now = chrono::Local::now().format("%Y-%m-%d_%H-%M-%S");
        let dir = PathBuf::from(&cfg.output.dir)
            .join("runs")
            .join(now.to_string());
        fs::create_dir_all(dir.join("snaps"))?;
        let open = |name: &str| -> std::io::Result<File> {
            File::create(dir.join(name))
        };
        let sink = Self {
            dir: Some(dir.clone()),
            telemetry: Some(open("telemetry.jsonl")?),
            detections: Some(open("detections.jsonl")?),
            raw_detections: Some(open("raw_detections.jsonl")?),
            commander: Some(open("commander.jsonl")?),
            gmc: Some(open("gmc.jsonl")?),
            perf: Some(open("perf.jsonl")?),
        };
        sink.write_session(cfg);
        Ok(sink)
    }

    fn write_session(&self, cfg: &AppConfig) {
        let Some(dir) = &self.dir else { return };
        let mut info = serde_json::json!({
            "started": chrono::Local::now().to_rfc3339(),
            "git_hash": env!("SYNERGY_GIT_HASH"),
            "build_time": env!("SYNERGY_BUILD_TIME"),
            "version": env!("CARGO_PKG_VERSION"),
        });
        // Окружение борта (лучше-что-есть-принцип).
        if let Ok(u) = std::process::Command::new("uname").arg("-a").output() {
            info["board"] =
                serde_json::Value::String(String::from_utf8_lossy(&u.stdout).trim().into());
        }
        // Полный конфиг прогона — главный ключ к воспроизводимости
        // (Debug-дамп: все поля структур конфига).
        info["config_debug"] = serde_json::Value::String(format!("{cfg:?}"));
        let _ = fs::write(dir.join("session.json"), serde_json::to_string_pretty(&info).unwrap_or_default());
    }

    #[inline]
    pub fn enabled(&self) -> bool {
        self.dir.is_some()
    }

    /// Каталог прогона (для снапшотов и отчёта).
    pub fn dir(&self) -> Option<&PathBuf> {
        self.dir.as_ref()
    }

    #[inline]
    pub fn telemetry(&mut self, line: &str) {
        if let Some(f) = self.telemetry.as_mut() {
            let _ = f.write_all(line.as_bytes());
        }
    }

    /// Детекция, прошедшая рабочий порог (как раньше detections.jsonl).
    #[inline]
    pub fn detection(&mut self, frame_seq: u64, class: u32, conf: f32, b: &common::BBox) {
        if let Some(f) = self.detections.as_mut() {
            let ts = now_ms();
            let _ = writeln!(
                f,
                "{{\"ts_ms\":{ts},\"frame_seq\":{frame_seq},\"class\":{class},\"conf\":{conf:.3},\"x\":{},\"y\":{},\"w\":{},\"h\":{}}}",
                b.x as i32, b.y as i32, b.w as i32, b.h as i32
            );
        }
    }

    /// Сырая детекция (NMS-выжившая, ДО рабочего порога) — материал для
    /// офлайн-выбора порога (L2).
    #[inline]
    pub fn raw_detection(&mut self, frame_seq: u64, class: u32, conf: f32, b: &common::BBox) {
        if let Some(f) = self.raw_detections.as_mut() {
            let _ = writeln!(
                f,
                "{{\"frame_seq\":{frame_seq},\"class\":{class},\"conf\":{conf:.3},\"x\":{},\"y\":{},\"w\":{},\"h\":{}}}",
                b.x as i32, b.y as i32, b.w as i32, b.h as i32
            );
        }
    }

    /// Тик контура наведения (L3): ошибка px, скорость цели, каналы.
    #[inline]
    pub fn commander_tick(
        &mut self,
        frame_seq: u64,
        mode: &str,
        err: (f32, f32),
        vel: (f32, f32),
        lead: (f32, f32),
        ch: &[u16; 16],
        armed: bool,
    ) {
        if let Some(f) = self.commander.as_mut() {
            let chs: Vec<String> = ch.iter().map(|c| c.to_string()).collect();
            let _ = writeln!(
                f,
                "{{\"ts_ms\":{},\"frame_seq\":{frame_seq},\"mode\":\"{mode}\",\"err\":[{:.1},{:.1}],\"vel\":[{:.1},{:.1}],\"lead\":[{:.1},{:.1}],\"ch\":[{}],\"armed\":{}}}",
                now_ms(), err.0, err.1, vel.0, vel.1, lead.0, lead.1,
                chs.join(","), armed
            );
        }
    }

    /// Оценка глобального сдвига кадра (L4) — вибропрофиль.
    #[inline]
    pub fn gmc(&mut self, frame_seq: u64, dx: f32, dy: f32) {
        if let Some(f) = self.gmc.as_mut() {
            let _ = writeln!(f, "{{\"frame_seq\":{frame_seq},\"dx\":{dx:.2},\"dy\":{dy:.2}}}");
        }
    }

    /// Периодическая сводка производительности/здоровья (L5).
    #[inline]
    pub fn perf(&mut self, line: &str) {
        if let Some(f) = self.perf.as_mut() {
            let _ = f.write_all(line.as_bytes());
        }
    }

    /// Снапшот кадра (diag-режим; каталог прогона/snaps).
    pub fn snapshot(&self, seq: u64, jpeg: &[u8]) {
        if let Some(dir) = &self.dir {
            let p = dir.join("snaps").join(format!("frame_{seq:06}.jpg"));
            let _ = fs::write(p, jpeg);
        }
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

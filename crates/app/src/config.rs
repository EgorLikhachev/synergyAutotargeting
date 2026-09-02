//! Конфигурация приложения (TOML). См. config.example.toml в корне репо.

use common::VideoConfig;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub camera: VideoConfig,
    pub detector: DetectorConfig,
    pub tracker: TrackerConfig,
    pub pipeline: PipelineConfig,
    pub output: OutputConfig,
    pub stream: StreamConfig,
    pub commander: CommanderConfig,
    pub synthetic: SyntheticConfig,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            camera: VideoConfig::default(),
            detector: DetectorConfig::default(),
            tracker: TrackerConfig::default(),
            pipeline: PipelineConfig::default(),
            output: OutputConfig::default(),
            stream: StreamConfig::default(),
            commander: CommanderConfig::default(),
            synthetic: SyntheticConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct DetectorConfig {
    /// Путь к .rknn модели (bkb model_5_dynamic_rk3588.rknn).
    pub model_path: String,
    /// Вход модели (для letterbox и динамических форм).
    pub input_size: u32,
    pub conf_threshold: f32,
    pub nms_threshold: f32,
    /// Имена классов модели (по эмпирическим данным M3; пусто — class_N).
    pub class_names: Vec<String>,
    /// Пер-класс пороги {class_id: conf}; класс без записи — conf_threshold.
    pub class_thresholds: std::collections::HashMap<u32, f32>,
}

impl Default for DetectorConfig {
    fn default() -> Self {
        Self {
            model_path: "models/model_5_dynamic_rk3588.rknn".into(),
            input_size: 640,
            conf_threshold: 0.45,
            nms_threshold: 0.45,
            class_names: Vec::new(),
            class_thresholds: std::collections::HashMap::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct TrackerConfig {
    /// Нейро-бэкенд трекера: "tract" (CPU) или "rknn" (NPU, фаза C).
    pub backend: String,
    /// Модели tract (CPU).
    pub backbone_path: String,
    /// Backbone для поиска 255×255.
    pub backbone_search_path: String,
    pub head_path: String,
    /// Модели RKNN (NPU; из tools/convert_nanotrack.py).
    pub rknn_backbone_z: String,
    pub rknn_backbone_x: String,
    pub rknn_head: String,
    /// Мы кормим RGB; true нужен только если источник вдруг BGR.
    pub swap_rb: bool,
    pub min_track_score: f32,
}

impl Default for TrackerConfig {
    fn default() -> Self {
        Self {
            backend: "tract".into(),
            backbone_path: "models/nanotrack_backbone_127.onnx".into(),
            backbone_search_path: "models/nanotrack_backbone_sim.onnx".into(),
            head_path: "models/nanotrack_head_sim.onnx".into(),
            rknn_backbone_z: "models/nanotrack_backbone_127.rknn".into(),
            rknn_backbone_x: "models/nanotrack_backbone_255.rknn".into(),
            rknn_head: "models/nanotrack_head.rknn".into(),
            swap_rb: false,
            min_track_score: 0.30,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct PipelineConfig {
    pub detect_every_n: u32,
    pub iou_confirm: f32,
    pub lost_patience: u32,
    pub min_detect_conf: f32,
    pub priority_classes: Vec<u32>,
    pub use_stabilizer: bool,
    /// Цифровая стабилизация (GMC) — жёсткий монтаж камеры.
    pub gmc: bool,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            detect_every_n: 10,
            iou_confirm: 0.3,
            lost_patience: 3,
            min_detect_conf: 0.45,
            priority_classes: Vec::new(),
            use_stabilizer: true,
            gmc: false,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct OutputConfig {
    pub dir: String,
    /// Сохранять OSD-снапшот каждые N кадров.
    pub snapshot_every: u32,
    /// Писать JSONL-телеметрию.
    pub telemetry: bool,
    /// Длительность прогона, секунды (0 — бесконечно до Ctrl+C).
    pub duration_secs: u64,
}

impl Default for OutputConfig {
    fn default() -> Self {
        Self {
            dir: "data".into(),
            snapshot_every: 30,
            telemetry: true,
            duration_secs: 0,
        }
    }
}

/// Живой MJPEG-стрим OSD (ADR-009): http://<ip>:<port>/ из браузера/VLC.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct StreamConfig {
    pub enabled: bool,
    /// Адрес привязки ("0.0.0.0:8080" — слушать все интерфейсы).
    pub bind: String,
    /// Push-режим: борт сам подключается к зрителю ("192.168.0.174:9000",
    /// пусто — выключено). Работает всегда, см. ADR-009 (quirk ядра).
    pub push_to: String,
    /// Отдавать каждый N-й кадр (2 ≈ 15 FPS при 30 камере).
    pub frame_div: u32,
    /// Качество JPEG (как у снапшотов).
    pub quality: u8,
    /// Запись стрима в файл (M-JPEG, конкатенация кадров); пусто — выкл.
    pub record: String,
}

impl Default for StreamConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bind: "0.0.0.0:8080".into(),
            push_to: String::new(),
            frame_div: 2,
            quality: 80,
            record: String::new(),
        }
    }
}

/// Коммандер наведения (фаза D, ADR-012): MSP v1 SET_RAW_RC поверх UART.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct CommanderConfig {
    pub enabled: bool,
    /// UART-устройство исполнителя (bkb: /dev/ttyS6).
    pub device: String,
    pub baud: u32,
    /// Частота отправки RC, Гц (bkb: 30).
    pub rate_hz: u32,
    /// Усиления осей (одинаковы для X/Y на старте).
    pub kp: f32,
    pub ki: f32,
    pub kd: f32,
    /// Мёртвая зона, px.
    pub deadband_px: f32,
    /// Slew-лимит, мкс/тик.
    pub slew_us: f32,
    /// Камера повёрнута на 90° — свап осей (bkb).
    pub swap_axes: bool,
    pub reverse_x: bool,
    pub reverse_y: bool,
    /// Постоянные каналы: throttle (ch3) и aux1 (ch4).
    pub throttle_us: u16,
    pub aux1_us: u16,
    /// Симулятор вместо UART (тесты контура без железа).
    pub simulate: bool,
    /// Упреждение наведения: горизонт (с, 0 — выкл) и сглаживание скорости.
    pub lead_s: f32,
    pub lead_alpha: f32,
    /// Скорость платформы при полном стике, px/с (фидфорвард упреждения).
    pub stick_rate_px_s: f32,
}

impl Default for CommanderConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            device: "/dev/ttyS6".into(),
            baud: 115200,
            rate_hz: 30,
            kp: 2.0,
            ki: 0.0,
            kd: 0.0,
            deadband_px: 6.0,
            slew_us: 8.0,
            swap_axes: false,
            reverse_x: false,
            reverse_y: false,
            throttle_us: 1310,
            aux1_us: 1950,
            simulate: false,
            lead_s: 0.0,
            lead_alpha: 0.25,
            stick_rate_px_s: 600.0,
        }
    }
}

/// Синтетический источник (--synthetic).
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct SyntheticConfig {
    /// Амплитуда тряски камеры, px (стенд GMC-стабилизации).
    pub shake_px: f32,
}

impl Default for SyntheticConfig {
    fn default() -> Self {
        Self { shake_px: 0.0 }
    }
}

impl AppConfig {
    pub fn load(path: &str) -> anyhow::Result<Self> {
        let raw = std::fs::read_to_string(path)?;
        Ok(toml::from_str(&raw)?)
    }
}

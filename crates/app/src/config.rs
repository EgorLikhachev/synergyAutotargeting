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
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            camera: VideoConfig::default(),
            detector: DetectorConfig::default(),
            tracker: TrackerConfig::default(),
            pipeline: PipelineConfig::default(),
            output: OutputConfig::default(),
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
}

impl Default for DetectorConfig {
    fn default() -> Self {
        Self {
            model_path: "models/model_5_dynamic_rk3588.rknn".into(),
            input_size: 640,
            conf_threshold: 0.45,
            nms_threshold: 0.45,
            class_names: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct TrackerConfig {
    /// Backbone для шаблона 127×127.
    pub backbone_path: String,
    /// Backbone для поиска 255×255.
    pub backbone_search_path: String,
    pub head_path: String,
    /// Мы кормим RGB; true нужен только если источник вдруг BGR.
    pub swap_rb: bool,
    pub min_track_score: f32,
}

impl Default for TrackerConfig {
    fn default() -> Self {
        Self {
            backbone_path: "models/nanotrack_backbone_127.onnx".into(),
            backbone_search_path: "models/nanotrack_backbone_sim.onnx".into(),
            head_path: "models/nanotrack_head_sim.onnx".into(),
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

impl AppConfig {
    pub fn load(path: &str) -> anyhow::Result<Self> {
        let raw = std::fs::read_to_string(path)?;
        Ok(toml::from_str(&raw)?)
    }
}

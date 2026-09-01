//! Трейт источника видео (порт из Autotargeting).

use async_trait::async_trait;
use common::Frame;
use thiserror::Error;
use tokio::sync::mpsc;

#[derive(Debug, Error)]
pub enum VideoCaptureError {
    #[error("ошибка открытия устройства: {0}")]
    DeviceOpen(String),

    #[error("ошибка конфигурации устройства: {0}")]
    DeviceConfig(String),

    #[error("ошибка захвата: {0}")]
    Capture(String),

    #[error("ошибка декодирования: {0}")]
    Decode(String),

    #[error("устройство отключено")]
    Disconnected,
}

pub type VideoResult<T> = std::result::Result<T, VideoCaptureError>;

/// Источник кадров: V4L2-устройство (реальный), каталог JPEG (replay),
/// синтетический генератор (тесты).
#[async_trait]
pub trait VideoSource: Send {
    /// Запустить захват. Возвращает канал кадров; закрытие приёмника
    /// останавливает захват.
    async fn start(&mut self) -> VideoResult<mpsc::Receiver<Frame>>;

    /// Остановить захват. Идемпотентно.
    async fn stop(&mut self) -> VideoResult<()>;

    /// Человекочитаемое имя (например "V4l2DirectSource(/dev/video0)").
    fn name(&self) -> &str;
}

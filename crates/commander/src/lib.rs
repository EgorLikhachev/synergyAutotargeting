//! Commander наведения (фаза D, ROADMAP): контур «цель − центр кадра» →
//! RC-каналы → исполнитель.
//!
//! Протокол и закон управления портированы из bkb (ADR-012):
//! - wire: MSP v1 SET_RAW_RC (Betaflight/INAV), UART 115200, 30 Гц;
//! - закон: нормированная ошибка + P/PID, мёртвая зона, slew-лимит,
//!   свап осей под повёрнутую камеру, ARM через MSP2_SET_ARMING.

pub mod law;
pub mod msp;
pub mod sim;
pub mod uart;

pub use law::{AimConfig, AimLaw, AxisParams};
pub use sim::{AimLink, NoopLink, PlatformSim};

/// Такт отправки команд (как в bkb), Гц.
pub const SEND_RATE_HZ: f32 = 30.0;

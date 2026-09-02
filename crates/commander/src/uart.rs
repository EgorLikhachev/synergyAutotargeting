//! UART-транспорт MSP (борт: /dev/ttyS*). serialport-rs — чистый Rust
//! поверх termios, соответствует ADR-001.

use anyhow::{Context, Result};
use serialport::SerialPort;

use crate::msp;
use crate::sim::AimLink;

/// UART-линк к полётному контроллеру (или иному MSP-исполнителю).
pub struct UartLink {
    port: Box<dyn SerialPort>,
    sent: u64,
}

impl UartLink {
    pub fn open(device: &str, baud: u32) -> Result<Self> {
        let port = serialport::new(device, baud)
            .timeout(std::time::Duration::from_millis(10))
            .open()
            .with_context(|| format!("открытие UART {device}:{baud}"))?;
        tracing::info!(device, baud, "UART командира открыт");
        Ok(Self { port, sent: 0 })
    }

    /// Отправить произвольный фрейм (например, ARM).
    pub fn send_raw(&mut self, frame: &[u8]) -> Result<()> {
        self.port.write_all(frame)?;
        self.sent += 1;
        Ok(())
    }

    pub fn sent_frames(&self) -> u64 {
        self.sent
    }
}

impl AimLink for UartLink {
    fn send_rc(&mut self, ch: &[u16; 16]) -> Result<()> {
        self.send_raw(&msp::set_raw_rc(ch))
    }
}

//! UART-транспорт MSP (борт: /dev/ttyS* или USB-VCP /dev/tty-fc).
//! serialport-rs — чистый Rust поверх termios, соответствует ADR-001.
//!
//! Телеметрия FC (ADR-021): отдельный поток читает MSP-ответы и раз в
//! 500 мс опрашивает MSP_STATUS/MSP_RC; снимок доступен приложению через
//! [`msp::FcTelemetry`] и уходит в статус UI. Запись (кадры RC от кадрового
//! цикла и запросы телеметрии) сериализуется общим мьютексом; чтение —
//! через try_clone порта, без конкуренции.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serialport::SerialPort;

use crate::msp::{self, FcTelemetry, MSP_RC, MSP_STATUS};
use crate::sim::AimLink;

/// Период опроса телеметрии FC (MSP_STATUS + MSP_RC = 12 байт запросов).
const TELEMETRY_POLL: Duration = Duration::from_millis(500);

/// UART-линк к полётному контроллеру (или иному MSP-исполнителю).
pub struct UartLink {
    writer: Arc<Mutex<Box<dyn SerialPort>>>,
    fc: Arc<Mutex<FcTelemetry>>,
    sent: u64,
}

impl UartLink {
    pub fn open(device: &str, baud: u32) -> Result<Self> {
        let port = serialport::new(device, baud)
            .timeout(Duration::from_millis(50))
            .open()
            .with_context(|| format!("открытие UART {device}:{baud}"))?;
        let writer = Arc::new(Mutex::new(port));
        let fc = Arc::new(Mutex::new(FcTelemetry::default()));
        let reader = writer
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .try_clone()
            .with_context(|| "клонирование порта для потока телеметрии")?;
        let (w2, fc2) = (writer.clone(), fc.clone());
        let dev = device.to_string();
        std::thread::Builder::new()
            .name("msp-telemetry".into())
            .spawn(move || telemetry_loop(dev, reader, w2, fc2))
            .ok()
            .context("запуск потока телеметрии")?;
        tracing::info!(device, baud, "UART командира открыт (телеметрия 2 Гц)");
        Ok(Self { writer, fc, sent: 0 })
    }

    /// Отправить произвольный фрейм (например, ARM).
    pub fn send_raw(&mut self, frame: &[u8]) -> Result<()> {
        self.writer
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .write_all(frame)?;
        self.sent += 1;
        Ok(())
    }

    pub fn sent_frames(&self) -> u64 {
        self.sent
    }

    /// Снимок телеметрии FC для статуса UI.
    pub fn fc_snapshot(&self) -> FcTelemetry {
        self.fc.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

impl AimLink for UartLink {
    fn send_rc(&mut self, ch: &[u16; 16]) -> Result<()> {
        self.send_raw(&msp::set_raw_rc(ch))
    }

    fn fc_telemetry(&self) -> Option<Arc<Mutex<FcTelemetry>>> {
        Some(self.fc.clone())
    }
}

/// Поток: читать ответы, периодически спрашивать STATUS/RC.
fn telemetry_loop(
    dev: String,
    mut reader: Box<dyn SerialPort>,
    writer: Arc<Mutex<Box<dyn SerialPort>>>,
    fc: Arc<Mutex<FcTelemetry>>,
) {
    let mut parser = msp::ReplyParser::new();
    let mut last_poll = Instant::now() - TELEMETRY_POLL; // спросить сразу
    loop {
        if last_poll.elapsed() >= TELEMETRY_POLL {
            let mut w = writer.lock().unwrap_or_else(|e| e.into_inner());
            if w.write_all(&msp::status_request()).is_err() {
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
            let _ = w.write_all(&msp::rc_request());
            last_poll = Instant::now();
        }
        let mut buf = [0u8; 128];
        match reader.read(&mut buf) {
            Ok(0) => {}
            Ok(n) => {
                for (cmd, payload) in parser.feed(&buf[..n]) {
                    let mut st = fc.lock().unwrap_or_else(|e| e.into_inner());
                    match cmd {
                        MSP_STATUS => {
                            if let Some(flags) = msp::parse_status_flags(&payload) {
                                st.flags = flags;
                                st.rx_ok = flags & msp::ARMING_FLAG_RC_ALIVE != 0;
                                tracing::debug!(dev = %dev, flags, "FC status");
                            }
                        }
                        MSP_RC => {
                            st.ch = msp::parse_rc(&payload);
                        }
                        _ => {} // ack'и SET_RAW_RC и прочее не разбираем
                    }
                    st.online = true;
                }
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(e) => {
                tracing::debug!(dev = %dev, error = %e, "FC read");
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

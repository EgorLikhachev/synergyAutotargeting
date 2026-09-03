//! Канал управления с операторским UI (ADR-016).
//!
//! Борт сам подключается к UI (исходящий TCP — работает всегда при
//! NPU-quirk ядра, ADR-009). Протокол — JSON-строки в обе стороны:
//!
//!   UI → борт:  {"t":"lock","x":..,"y":..,"size":..} | {"t":"arm","on":true}
//!               | {"t":"stop"} | {"t":"ping"}
//!   борт → UI:  {"t":"status",...} каждый кадр | {"t":"hello"}
//!
//! Fail-safe: при armed обрыв соединения или тишина UI > 1 с — борт
//! сам выполняет СТОП (безопасность наведения).

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Команда от оператора.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum UiCmd {
    /// Захват цели: центр (x, y) в пикселях кадра + размер стороны ROI.
    Lock { x: f32, y: f32, size: f32 },
    /// Разрешить/запретить контур наведения.
    Arm { on: bool },
    /// Мгновенный СТОП: центр стиков + разарм.
    Stop,
}

/// Общие слоты между потоком канала и приложением.
struct Shared {
    cmd: Mutex<Option<UiCmd>>,
    status: Mutex<Option<String>>,
    /// armed-состояние для fail-safe (обновляет CommanderCtx).
    armed: AtomicBool,
    last_msg_ms: AtomicU64,
}

pub struct ControlLink {
    shared: Arc<Shared>,
    connected: Arc<AtomicBool>,
    pub sent_status: Arc<AtomicU64>,
}

impl ControlLink {
    /// Подключаться к `addr` (например "192.168.0.174:9010"), реконнект 3 с.
    pub fn start(addr: &str) -> Self {
        let shared = Arc::new(Shared {
            cmd: Mutex::new(None),
            status: Mutex::new(None),
            armed: AtomicBool::new(false),
            last_msg_ms: AtomicU64::new(0),
        });
        let connected = Arc::new(AtomicBool::new(false));
        let sent_status = Arc::new(AtomicU64::new(0));
        let (sh, conn, sent) = (shared.clone(), connected.clone(), sent_status.clone());
        let addr = addr.to_string();
        let _ = std::thread::Builder::new()
            .name("ui-control".into())
            .spawn(move || {
                eprintln!("[UI-CTRL] запущен, цель {addr}");
                loop {
                    if let Ok(mut sock) = TcpStream::connect(&addr) {
                        let _ = sock.set_nodelay(true);
                        let _ = sock.set_read_timeout(Some(Duration::from_millis(50)));
                        let _ = sock.write_all(b"{\"t\":\"hello\"}\n");
                        let _ = sock.flush();
                        conn.store(true, Ordering::Relaxed);
                        eprintln!("[UI-CTRL] подключён к {addr}");
                        let read_sock = match sock.try_clone() {
                            Ok(s) => s,
                            Err(_) => {
                                conn.store(false, Ordering::Relaxed);
                                std::thread::sleep(Duration::from_secs(3));
                                continue;
                            }
                        };
                        let mut reader = BufReader::new(read_sock);
                        let mut line = String::new();
                        loop {
                            line.clear();
                            match reader.read_line(&mut line) {
                                Ok(0) => break, // EOF — соединение закрыто
                                Ok(_) => {
                                    sh.last_msg_ms.store(now_ms(), Ordering::Relaxed);
                                    if let Some(cmd) = parse_cmd(&line) {
                                        *sh.cmd.lock().unwrap() = Some(cmd);
                                    }
                                }
                                Err(ref e)
                                    if e.kind() == std::io::ErrorKind::WouldBlock => {}
                                Err(_) => break,
                            }
                            // fail-safe: armed + тишина > 1 с → СТОП
                            if sh.armed.load(Ordering::Relaxed) {
                                let last = sh.last_msg_ms.load(Ordering::Relaxed);
                                if last > 0 && now_ms().saturating_sub(last) > 1000 {
                                    eprintln!(
                                        "[UI-CTRL] FAIL-SAFE: тишина UI при armed → СТОП"
                                    );
                                    *sh.cmd.lock().unwrap() = Some(UiCmd::Stop);
                                    sh.armed.store(false, Ordering::Relaxed);
                                }
                            }
                            // статус: замещаемый слот (свежий кадр важнее очереди)
                            if let Some(st) = sh.status.lock().unwrap().take() {
                                if sock.write_all(st.as_bytes()).is_err() {
                                    break;
                                }
                                sent.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                    }
                    conn.store(false, Ordering::Relaxed);
                    // обрыв при armed → мгновенный СТОП
                    if sh.armed.load(Ordering::Relaxed) {
                        eprintln!("[UI-CTRL] FAIL-SAFE: обрыв при armed → СТОП");
                        *sh.cmd.lock().unwrap() = Some(UiCmd::Stop);
                        sh.armed.store(false, Ordering::Relaxed);
                    }
                    std::thread::sleep(Duration::from_secs(3));
                }
            });
        Self { shared, connected, sent_status }
    }

    /// Забрать очередную команду.
    pub fn take_cmd(&self) -> Option<UiCmd> {
        self.shared.cmd.lock().unwrap().take()
    }

    /// Очередь статуса к отправке (замещаемая).
    pub fn send_status(&self, json_line: String) {
        *self.shared.status.lock().unwrap() = Some(json_line);
    }

    /// Признак armed для fail-safe.
    pub fn set_armed(&self, on: bool) {
        self.shared.armed.store(on, Ordering::Relaxed);
        if on {
            self.shared.last_msg_ms.store(now_ms(), Ordering::Relaxed);
        }
    }

    pub fn connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Разбор команды из JSON-строки.
fn parse_cmd(line: &str) -> Option<UiCmd> {
    let v: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
    match v.get("t")?.as_str()? {
        "lock" => Some(UiCmd::Lock {
            x: v.get("x")?.as_f64()? as f32,
            y: v.get("y")?.as_f64()? as f32,
            size: v.get("size")?.as_f64()? as f32,
        }),
        "arm" => Some(UiCmd::Arm { on: v.get("on")?.as_bool()? }),
        "stop" => Some(UiCmd::Stop),
        "ping" => None,
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_all_commands() {
        assert_eq!(
            parse_cmd(r#"{"t":"lock","x":320.5,"y":240,"size":100}"#),
            Some(UiCmd::Lock { x: 320.5, y: 240.0, size: 100.0 })
        );
        assert_eq!(
            parse_cmd(r#"{"t":"arm","on":true}"#),
            Some(UiCmd::Arm { on: true })
        );
        assert_eq!(parse_cmd(r#"{"t":"stop"}"#), Some(UiCmd::Stop));
        assert_eq!(parse_cmd(r#"{"t":"ping"}"#), None);
        assert_eq!(parse_cmd("мусор"), None);
        assert_eq!(parse_cmd(r#"{"t":"lock","x":"строка"}"#), None);
    }
}

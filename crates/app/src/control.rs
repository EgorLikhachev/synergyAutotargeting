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
    /// Снять трекинг: цель сбрасывается, авто-захват выключен до
    /// следующего lock (наведение уходит в центры).
    Unlock,
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
    /// Подключаться к `ui_addr` (например "192.168.0.174:9010"), реконнект 3 с.
    /// Непустой `token` включает аутентификацию команд (safety §6.3).
    pub fn start(ui_addr: &str, token: &str) -> Self {
        let shared = Arc::new(Shared {
            cmd: Mutex::new(None),
            status: Mutex::new(None),
            armed: AtomicBool::new(false),
            last_msg_ms: AtomicU64::new(0),
        });
        let connected = Arc::new(AtomicBool::new(false));
        let sent_status = Arc::new(AtomicU64::new(0));
        let (sh, conn, sent) = (shared.clone(), connected.clone(), sent_status.clone());
        let addr = ui_addr.to_string();
        let token = token.to_string();
        let _ = std::thread::Builder::new()
            .name("ui-control".into())
            .spawn(move || {
                eprintln!("[UI-CTRL] запущен, цель {addr}");
                let sa: std::net::SocketAddr = match addr.parse() {
                    Ok(v) => v,
                    Err(_) => {
                        eprintln!("[UI-CTRL] некорректный адрес {addr}");
                        return;
                    }
                };
                loop {
                    // connect_timeout: блокирующий connect при мёртвом зрителе
                    // висит ~2 мин и не даёт быстро переподключиться.
                    let sock = TcpStream::connect_timeout(&sa, Duration::from_secs(2));
                    if let Ok(mut sock) = sock {
                        let _ = sock.set_nodelay(true);
                        let _ = sock.set_read_timeout(Some(Duration::from_millis(50)));
                        // Таймаут записи: «серый» клиент (не читает, но и не
                        // рвёт TCP) иначе вешает write_all на ~15 мин TCP-
                        // ретрансмиссий — цикл стоит, STOP не читается и
                        // dead-man не проверяется. Обрыв записи = разрыв
                        // канала → существующий путь делает СТОП при armed.
                        let _ = sock.set_write_timeout(Some(Duration::from_millis(500)));
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
                                Ok(_) => match parse_line(&line, &token) {
                                    LineIn::Cmd(cmd) => {
                                        sh.last_msg_ms.store(now_ms(), Ordering::Relaxed);
                                        *sh.cmd.lock().unwrap() = Some(cmd);
                                    }
                                    // авторизованный ping продлевает dead-man,
                                    // чужой трафик — нет (safety §6.3).
                                    LineIn::Ping => {
                                        sh.last_msg_ms.store(now_ms(), Ordering::Relaxed);
                                    }
                                    LineIn::Reject => {}
                                },
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

/// Классификация входной строки канала.
enum LineIn {
    Cmd(UiCmd),
    /// Авторизованный ping: команды нет, но живость есть.
    Ping,
    /// Мусор, неизвестная команда или провал аутентификации.
    Reject,
}

/// Разбор команды из JSON-строки. При непустом `token` каждая строка (включая
/// ping) обязана нести "auth" с точным совпадением — иначе Reject: чужой
/// трафик не попадает в слот команд и НЕ продлевает dead-man таймер.
fn parse_line(line: &str, token: &str) -> LineIn {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
        return LineIn::Reject;
    };
    if !token.is_empty() && v.get("auth").and_then(|a| a.as_str()) != Some(token) {
        return LineIn::Reject;
    }
    match v.get("t").and_then(|t| t.as_str()) {
        Some("lock") => {
            let (Some(x), Some(y), Some(size)) = (
                v.get("x").and_then(|f| f.as_f64()),
                v.get("y").and_then(|f| f.as_f64()),
                v.get("size").and_then(|f| f.as_f64()),
            ) else {
                return LineIn::Reject;
            };
            LineIn::Cmd(UiCmd::Lock { x: x as f32, y: y as f32, size: size as f32 })
        }
        Some("arm") => match v.get("on").and_then(|b| b.as_bool()) {
            Some(on) => LineIn::Cmd(UiCmd::Arm { on }),
            None => LineIn::Reject,
        },
        Some("stop") => LineIn::Cmd(UiCmd::Stop),
        Some("unlock") => LineIn::Cmd(UiCmd::Unlock),
        Some("ping") => LineIn::Ping,
        _ => LineIn::Reject,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(line: &str) -> Option<UiCmd> {
        match parse_line(line, "") {
            LineIn::Cmd(c) => Some(c),
            _ => None,
        }
    }

    #[test]
    fn parse_all_commands() {
        assert_eq!(
            cmd(r#"{"t":"lock","x":320.5,"y":240,"size":100}"#),
            Some(UiCmd::Lock { x: 320.5, y: 240.0, size: 100.0 })
        );
        assert_eq!(cmd(r#"{"t":"arm","on":true}"#), Some(UiCmd::Arm { on: true }));
        assert_eq!(cmd(r#"{"t":"stop"}"#), Some(UiCmd::Stop));
        assert_eq!(cmd(r#"{"t":"unlock"}"#), Some(UiCmd::Unlock));
        assert!(matches!(parse_line(r#"{"t":"ping"}"#, ""), LineIn::Ping));
        assert!(matches!(cmd("мусор"), None));
        assert!(matches!(cmd(r#"{"t":"lock","x":"строка"}"#), None));
    }

    #[test]
    fn auth_gate() {
        // токен выключен: всё работает как раньше
        assert!(matches!(parse_line(r#"{"t":"arm","on":true}"#, ""), LineIn::Cmd(_)));
        // токен включен: без auth / с чужим auth — Reject
        let t = "bench-secret";
        assert!(matches!(parse_line(r#"{"t":"arm","on":true}"#, t), LineIn::Reject));
        assert!(matches!(parse_line(r#"{"t":"arm","on":true,"auth":"nope"}"#, t), LineIn::Reject));
        // с верным auth — команда доходит, ping — живость
        assert!(matches!(
            parse_line(r#"{"t":"arm","on":true,"auth":"bench-secret"}"#, t),
            LineIn::Cmd(UiCmd::Arm { on: true })
        ));
        assert!(matches!(parse_line(r#"{"t":"ping","auth":"bench-secret"}"#, t), LineIn::Ping));
        // чужой ping НЕ считается живостью
        assert!(matches!(parse_line(r#"{"t":"ping","auth":"nope"}"#, t), LineIn::Reject));
    }
}

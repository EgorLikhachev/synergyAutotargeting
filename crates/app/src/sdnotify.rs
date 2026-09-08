//! Минимальный sd_notify на std, без libc/libsystemd: `READY=1` при старте и
//! `WATCHDOG=1` по расписанию — третья независимая ступень безопасности
//! (safety_compliance §6.2, фаза E ROADMAP): зависание главного цикла =
//! systemd убивает и перезапускает сервис в безопасном состоянии (armed=false).
//!
//! Активен только под systemd (`Type=notify` + `WatchdogSec` в
//! tools/synergy.service задаёт env `NOTIFY_SOCKET`/`WATCHDOG_USEC`);
//! вне systemd (локальный запуск, тесты, Windows) — тихий no-op.

use std::time::{Duration, Instant};

pub struct SdWatchdog {
    #[cfg(unix)]
    sock: Option<std::os::unix::net::UnixDatagram>,
    interval: Option<Duration>,
    last: Instant,
}

// Примечание API: UnixDatagram::connect/send_to в std принимают только
// AsRef<Path> — работа через SocketAddr (в т.ч. abstract '@'-namespace)
// этим API не поддерживается. systemd >= 236 (на борту 257) выставляет
// NOTIFY_SOCKET обычным путём (/run/systemd/notify) — этого достаточно.

impl Default for SdWatchdog {
    fn default() -> Self {
        Self::from_env()
    }
}

impl SdWatchdog {
    pub fn from_env() -> Self {
        let usec = std::env::var("WATCHDOG_USEC")
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .unwrap_or(0);
        // systemd рекомендует пинать сторожа вдвое чаще WatchdogSec.
        let interval = (usec > 0).then(|| Duration::from_micros((usec / 2).max(250_000)));
        #[cfg(unix)]
        {
            let sock = std::env::var_os("NOTIFY_SOCKET").and_then(|p| {
                let sock = try_socket(&p).ok();
                // сокет уведомлений не наследуем дочерним процессам
                std::env::remove_var("NOTIFY_SOCKET");
                sock
            });
            Self { sock, interval, last: Instant::now() }
        }
        #[cfg(not(unix))]
        Self { interval, last: Instant::now() }
    }

    pub fn ready(&mut self) {
        self.send(b"READY=1");
    }

    /// Вызывать каждый кадр: шлёт WATCHDOG=1 не чаще WatchdogSec/2.
    pub fn kick(&mut self) {
        let Some(iv) = self.interval else { return };
        if self.last.elapsed() >= iv {
            self.last = Instant::now();
            self.send(b"WATCHDOG=1");
        }
    }

    #[cfg(unix)]
    fn send(&self, msg: &[u8]) {
        if let Some(s) = &self.sock {
            let _ = s.send(msg);
        }
    }

    #[cfg(not(unix))]
    fn send(&self, _msg: &[u8]) {}
}

/// Подключаемся к сокету уведомлений systemd обычным путём; дальше —
/// plain send() без адреса.
#[cfg(unix)]
fn try_socket(path: &std::ffi::OsStr) -> std::io::Result<std::os::unix::net::UnixDatagram> {
    use std::os::unix::net::UnixDatagram;
    let sock = UnixDatagram::unbound()?;
    sock.connect(std::path::Path::new(path))?;
    Ok(sock)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn disabled_without_env() {
        // NOTIFY_SOCKET уже изъят из окружения конструктором — сторож молчит.
        let mut wd = SdWatchdog::from_env();
        assert!(wd.interval.is_none() || std::env::var_os("NOTIFY_SOCKET").is_none());
        wd.ready();
        wd.kick(); // no-op, не паникует
    }

    #[test]
    fn sends_watchdog_to_socket() {
        use std::os::unix::net::UnixDatagram;
        let dir = std::env::temp_dir().join(format!("sdnotify-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("notify.sock");
        let _ = std::fs::remove_file(&path);
        let listener = UnixDatagram::bind(&path).unwrap();
        std::env::set_var("NOTIFY_SOCKET", &path);
        std::env::set_var("WATCHDOG_USEC", "300000");
        let mut wd = SdWatchdog::from_env();
        wd.ready();
        wd.kick();
        let mut buf = [0u8; 64];
        let (n, _) = listener.recv_from(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"READY=1");
        let (n, _) = listener.recv_from(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"WATCHDOG=1");
        let _ = std::fs::remove_file(&path);
    }
}

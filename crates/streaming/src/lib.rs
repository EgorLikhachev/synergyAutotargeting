//! MJPEG-over-HTTP сервер (multipart/x-mixed-replace) на чистом std.
//!
//! Живое наблюдение за OSD из любого браузера/VLC/ffplay: `http://<ip>:<port>/`.
//! Кодирует кадры производитель (app, jpeg-encoder), сюда приходят готовые
//! JPEG-байты; сервер рассылает их всем подключённым клиентам, семантика —
//! «всегда свежий кадр»: медленный клиент получает новый кадр, а не копит
//! очередь старых.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

/// Канал «последний кадр выигрывает»: send затирает не прочитанный элемент,
/// потребитель блокирующе ждёт свежий. Drop отправителя закрывает канал.
pub struct LatestSender<T> {
    state: Arc<ChannelState<T>>,
}
pub struct LatestReceiver<T> {
    state: Arc<ChannelState<T>>,
}

struct ChannelState<T> {
    inner: Mutex<ChannelSlot<T>>,
    cv: Condvar,
}
struct ChannelSlot<T> {
    item: Option<T>,
    generation: u64,
    closed: bool,
}

pub fn latest_channel<T>() -> (LatestSender<T>, LatestReceiver<T>) {
    let state = Arc::new(ChannelState {
        inner: Mutex::new(ChannelSlot { item: None, generation: 0, closed: false }),
        cv: Condvar::new(),
    });
    (
        LatestSender { state: state.clone() },
        LatestReceiver { state },
    )
}

impl<T> LatestSender<T> {
    /// Положить кадр, затерев предыдущий непрочитанный.
    pub fn send(&self, item: T) {
        let mut slot = self.state.inner.lock().unwrap();
        slot.item = Some(item);
        slot.generation += 1;
        self.state.cv.notify_all();
    }
}

impl<T> Drop for LatestSender<T> {
    fn drop(&mut self) {
        let mut slot = self.state.inner.lock().unwrap();
        slot.closed = true;
        self.state.cv.notify_all();
    }
}

impl<T> LatestReceiver<T> {
    /// Ждать кадр новее последнего прочитанного этим потоком.
    /// None — канал закрыт (отправитель умер) и кадров больше нет.
    pub fn recv(&self) -> Option<T> {
        let mut slot = self.state.inner.lock().unwrap();
        loop {
            if let Some(item) = slot.item.take() {
                return Some(item);
            }
            if slot.closed {
                return None;
            }
            slot = self.state.cv.wait(slot).unwrap();
        }
    }
}

// ---------------------------------------------------------------------------
// MJPEG-сервер
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct MjpegServer {
    hub: Arc<Hub>,
    port: u16,
}

struct Hub {
    inner: Mutex<HubInner>,
    cv: Condvar,
    /// Счётчик отданных кадров (для замеров битрейта/частоты).
    pub served_frames: AtomicU64,
}

struct HubInner {
    /// Свежайший JPEG (Arc — разделяется между клиентами без копий).
    latest: Option<Arc<Vec<u8>>>,
    generation: u64,
    clients: usize,
}

impl MjpegServer {
    /// Запустить сервер (поток accept + поток на клиента).
    pub fn start(bind: &str) -> std::io::Result<Self> {
        let listener = TcpListener::bind(bind)?;
        let port = listener.local_addr()?.port();
        let hub = Arc::new(Hub {
            inner: Mutex::new(HubInner { latest: None, generation: 0, clients: 0 }),
            cv: Condvar::new(),
            served_frames: AtomicU64::new(0),
        });
        let hub_accept = hub.clone();
        std::thread::Builder::new()
            .name("mjpeg-accept".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    match stream {
                        Ok(s) => {
                            eprintln!("[MJPEG] accept: {}", s.peer_addr().map(|a| a.to_string()).unwrap_or_default());
                            let hub = hub_accept.clone();
                            let _ = std::thread::Builder::new()
                                .name("mjpeg-client".into())
                                .spawn(move || serve_client(s, hub));
                        }
                        Err(e) => {
                            eprintln!("[MJPEG] accept ошибка: {e}");
                            break;
                        }
                    }
                }
            })?;
        Ok(Self { hub, port })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// Есть ли подключённые зрители (решает, кодировать ли кадр).
    pub fn clients(&self) -> usize {
        self.hub.inner.lock().unwrap().clients
    }

    pub fn served_frames(&self) -> u64 {
        self.hub.served_frames.load(Ordering::Relaxed)
    }

    /// Разослать кадр всем зрителям (медленный клиент просто пропустит старые).
    pub fn push_jpeg(&self, jpeg: Vec<u8>) {
        let mut inner = self.hub.inner.lock().unwrap();
        inner.latest = Some(Arc::new(jpeg));
        inner.generation += 1;
        self.hub.cv.notify_all();
    }
}

fn serve_client(mut stream: TcpStream, hub: Arc<Hub>) {
    let peer = stream.peer_addr().map(|a| a.to_string()).unwrap_or_default();
    let _ = stream.set_nodelay(true);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    // Съесть HTTP-запрос; путь решает, что отдавать.
    let mut buf = [0u8; 2048];
    let n = stream.read(&mut buf).unwrap_or(0);
    let req = String::from_utf8_lossy(&buf[..n.min(buf.len())]).into_owned();
    eprintln!("[MJPEG] {peer}: connect, {} байт: {}", req.lines().next().unwrap_or("").len(), req.lines().next().unwrap_or(""));

    // Прямой multipart в адресной строке часть браузеров рендерит стопкой
    // кадров («сетка»). Обёртка <img> заставляет заменять кадры по одному.
    if !req.starts_with("GET /stream") {
        const PAGE: &str = "<!DOCTYPE html><html><head><title>synergy OSD</title>\
<style>html,body{margin:0;height:100%;background:#000;display:flex;\
align-items:center;justify-content:center}img{max-width:100%;max-height:100%}\
</style></head><body><img src=\"/stream\" alt=\"stream\"></body></html>";
        let resp = format!(
            "HTTP/1.0 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\
             Content-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{}",
            PAGE.len(),
            PAGE
        );
        let _ = stream.write_all(resp.as_bytes());
        let _ = stream.flush();
        eprintln!("[MJPEG] {peer}: отдал HTML-обёртку");
        return;
    }

    let head = b"HTTP/1.0 200 OK\r\n\
                Content-Type: multipart/x-mixed-replace; boundary=frame\r\n\
                Cache-Control: no-store\r\n\
                Connection: close\r\n\r\n";
    if stream.write_all(head).is_err() {
        eprintln!("[MJPEG] {peer}: head не записан, закрываю");
        return;
    }

    {
        let mut inner = hub.inner.lock().unwrap();
        inner.clients += 1;
    }
    eprintln!("[MJPEG] {peer}: стрим запущен, клиент зарегистрирован");

    let mut seen: u64 = 0;
    loop {
        // Ждать кадр новее seen; таймаут — чтобы заметить ушедшего клиента
        // (write ниже упадёт) даже когда продюсер молчит.
        let jpeg = {
            let mut inner = hub.inner.lock().unwrap();
            loop {
                if inner.generation > seen {
                    seen = inner.generation;
                    break inner.latest.clone();
                }
                let (guard, timeout) = hub
                    .cv
                    .wait_timeout(inner, Duration::from_secs(5))
                    .unwrap();
                inner = guard;
                if timeout.timed_out() && inner.generation <= seen {
                    // Нет кадров 5 с — проверить клиента пустым разделителем.
                    drop(inner);
                    if stream.write_all(b"").is_err() {
                        let mut g = hub.inner.lock().unwrap();
                        g.clients = g.clients.saturating_sub(1);
                        return;
                    }
                    inner = hub.inner.lock().unwrap();
                }
            }
        };
        let Some(jpeg) = jpeg else { continue };
        let ok = write!(stream, "--frame\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n", jpeg.len())
            .and_then(|_| stream.write_all(&jpeg))
            .and_then(|_| stream.write_all(b"\r\n"))
            .and_then(|_| stream.flush())
            .is_ok();
        if ok {
            hub.served_frames.fetch_add(1, Ordering::Relaxed);
        } else {
            eprintln!("[MJPEG] {peer}: обрыв на записи кадра");
            let mut inner = hub.inner.lock().unwrap();
            inner.clients = inner.clients.saturating_sub(1);
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latest_channel_replaces_stale() {
        let (tx, rx) = latest_channel::<u32>();
        tx.send(1);
        tx.send(2); // затирает 1
        assert_eq!(rx.recv(), Some(2));
        drop(tx);
        assert_eq!(rx.recv(), None);
    }

    #[test]
    fn server_serves_multipart_stream() {
        let srv = MjpegServer::start("127.0.0.1:0").expect("bind");
        let port = srv.port();
        let pusher = std::thread::spawn(move || {
            for i in 0..5 {
                srv.push_jpeg(vec![i; 64]);
                std::thread::sleep(Duration::from_millis(20));
            }
            std::thread::sleep(Duration::from_millis(200));
        });
        // Как браузер: сначала страница-обёртка, потом <img> тянет /stream.
        let mut page = TcpStream::connect(("127.0.0.1", port)).expect("connect");
        page.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        page.write_all(b"GET / HTTP/1.1\r\nHost: test\r\n\r\n").unwrap();
        let mut page_buf = [0u8; 8192];
        let pn = page.read(&mut page_buf).expect("html ответ");
        let page_str = String::from_utf8_lossy(&page_buf[..pn]).into_owned();
        assert!(page_str.starts_with("HTTP/1.0 200 OK"), "page={page_str}");
        assert!(page_str.contains("<img src=\"/stream\""), "нет img-обёртки");

        let mut s = TcpStream::connect(("127.0.0.1", port)).expect("connect");
        s.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        s.write_all(b"GET /stream HTTP/1.1\r\nHost: test\r\n\r\n").unwrap();
        let mut all = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            match s.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    all.extend_from_slice(&chunk[..n]);
                    let frames = all
                        .windows(b"Content-Type: image/jpeg".len())
                        .filter(|w| *w == b"Content-Type: image/jpeg")
                        .count();
                    if frames >= 5 {
                        break;
                    }
                }
            }
        }
        pusher.join().unwrap();
        let head = String::from_utf8_lossy(&all[..all.len().min(200)]).into_owned();
        assert!(head.starts_with("HTTP/1.0 200 OK"), "head={head}");
        assert!(head.contains("multipart/x-mixed-replace"));
        let frames = all
            .windows(b"Content-Type: image/jpeg".len())
            .filter(|w| *w == b"Content-Type: image/jpeg")
            .count();
        assert!(frames >= 2, "получено кадров: {frames}");
    }
}

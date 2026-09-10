//! Сеть операторского приложения: приём видео-пуша (:9000) и
//! контрольного канала (:9010), команда — JSON-строки (ADR-016).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Deserialize;

/// Команда борту.
#[derive(Debug, Clone, Copy)]
pub enum UiCommand {
    Lock { x: f32, y: f32, size: f32 },
    Arm { on: bool },
    Stop,
    /// Снять трекинг (цель сбрасывается, авто-захват до следующего lock).
    Unlock,
}

impl UiCommand {
    /// `token` — общий секрет канала (safety §6.3), на пульте берётся из env
    /// SYNERGY_TOKEN; пустая строка = аутентификация на борту выключена.
    fn to_json(self, token: &str) -> String {
        let auth = if token.is_empty() {
            String::new()
        } else {
            format!(",\"auth\":\"{token}\"")
        };
        match self {
            UiCommand::Lock { x, y, size } => {
                format!("{{\"t\":\"lock\",\"x\":{x},\"y\":{y},\"size\":{size}{auth}}}\n")
            }
            UiCommand::Arm { on } => format!("{{\"t\":\"arm\",\"on\":{on}{auth}}}\n"),
            UiCommand::Stop => format!("{{\"t\":\"stop\"{auth}}}\n"),
            UiCommand::Unlock => format!("{{\"t\":\"unlock\"{auth}}}\n"),
        }
    }
}

/// Статус от борта (serde-парсинг JSON-строки).
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Status {
    #[serde(default)]
    pub frame_seq: u64,
    #[serde(default)]
    pub mode: String,
    #[serde(default)]
    pub score: f32,
    #[serde(default)]
    pub fps: f32,
    #[serde(default)]
    pub e2e_ms: f32,
    #[serde(default, rename = "box")]
    pub box_xywh: Option<[i32; 4]>,
    #[serde(default)]
    pub dets: Vec<DetsEntry>,
    #[serde(default)]
    pub armed: bool,
    /// Телеметрия FC (ADR-021): связь борт↔полётник и видимость RC-потока.
    #[serde(default)]
    pub fc: Option<FcState>,
}

/// Состояние FC из статуса борта.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct FcState {
    #[serde(default)]
    pub online: bool,
    #[serde(default)]
    pub rx_ok: bool,
    #[serde(default)]
    #[allow(dead_code)] // протокольное поле ADR-021 (flags FC), читается в ui_sim/журналах
    pub flags: u32,
    #[serde(default)]
    pub ch: Vec<u16>,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)] // поле 4 (score) приходит в строке статуса, но пока не отображается
pub struct DetsEntry(
    pub f32,
    pub f32,
    pub f32,
    pub f32,
    #[serde(default)] pub f32,
);

/// Декодированный кадр для отрисовки.
pub struct VideoFrame {
    pub version: u64,
    pub rgba: Vec<u8>,
    /// Фактический размер кадра (борт может прислать не 640×480).
    pub w: usize,
    pub h: usize,
}

/// Сводка активной/завершённой записи.
#[derive(Clone, Default)]
pub struct RecStats {
    pub frames: u64,
    pub bytes: u64,
    /// Кадры, не попавшие в запись (диск не успевал — канал полон).
    pub dropped: u64,
    /// Ошибка записи (диск переполнен и т.п.) — запись останавливается.
    pub error: Option<String>,
    pub started: Option<Instant>,
}

/// Запись стрима в .mjpg: приёмный поток отдаёт сырые JPEG в канал,
/// поток-писатель дописывает их в файл. Конкатенация JPEG — это ровно
/// replay-формат борта (--replay) и обычный M-JPEG для VLC: без
/// перекодирования, копия байтов стрима.
///
/// Долговечность: писатель флашит буфер раз в секунду; stop() ждёт
/// завершения писателя (джойн), так что закрытие окна не теряет хвост.
pub struct Recorder {
    tx: Mutex<Option<std::sync::mpsc::SyncSender<Vec<u8>>>>,
    path: Mutex<Option<PathBuf>>,
    stats: Arc<Mutex<RecStats>>,
    writer: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl Recorder {
    pub fn new() -> Self {
        Self {
            tx: Mutex::new(None),
            path: Mutex::new(None),
            stats: Arc::new(Mutex::new(RecStats::default())),
            writer: Mutex::new(None),
        }
    }

    pub fn is_recording(&self) -> bool {
        self.tx.lock().unwrap_or_else(|e| e.into_inner()).is_some()
    }

    pub fn stats(&self) -> RecStats {
        self.stats.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Начать запись в файл `dir/synergy_ГГГГММДД_ЧЧММСС.mjpg`.
    pub fn start(&self, dir: &std::path::Path) -> Result<PathBuf, String> {
        if self.is_recording() {
            return Err("запись уже идёт".into());
        }
        std::fs::create_dir_all(dir).map_err(|e| format!("каталог записи: {e}"))?;
        let path = dir.join(format!("synergy_{}.mjpg", timestamp_local()));
        let file = std::fs::File::create(&path).map_err(|e| format!("файл записи: {e}"))?;
        let (tx, rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(240);
        let stats = self.stats.clone();
        *stats.lock().unwrap_or_else(|e| e.into_inner()) =
            RecStats { started: Some(Instant::now()), ..Default::default() };
        let handle = std::thread::Builder::new()
            .name("rec-writer".into())
            .spawn(move || {
                let mut w = std::io::BufWriter::with_capacity(4 * 1024 * 1024, file);
                loop {
                    match rx.recv_timeout(Duration::from_secs(1)) {
                        Ok(frame) => {
                            if let Err(e) = w.write_all(&frame) {
                                let mut s = stats.lock().unwrap_or_else(|e| e.into_inner());
                                s.error = Some(format!("запись на диск: {e}"));
                                break;
                            }
                            let mut s = stats.lock().unwrap_or_else(|e| e.into_inner());
                            s.frames += 1;
                            s.bytes += frame.len() as u64;
                        }
                        // таймаут: канал пуст ≥1 с — флашим, чтобы хвост
                        // не копился в буфере (durability при закрытии окна)
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                            let _ = w.flush();
                        }
                        // stop() разорвал канал — флаш и закрытие при drop
                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
                let _ = w.flush();
            })
            .map_err(|e| format!("поток записи: {e}"))?;
        *self.path.lock().unwrap_or_else(|e| e.into_inner()) = Some(path.clone());
        *self.writer.lock().unwrap_or_else(|e| e.into_inner()) = Some(handle);
        *self.tx.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx);
        Ok(path)
    }

    /// Остановить запись и дождаться, пока писатель флашнет файл
    /// (буфер ≤4 МиБ — джойн занимает миллисекунды). Вернуть путь файла.
    pub fn stop(&self) -> Option<PathBuf> {
        let tx = self.tx.lock().unwrap_or_else(|e| e.into_inner()).take();
        drop(tx); // разрывает канал → писатель флашит и выходит
        if let Some(h) = self.writer.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = h.join();
        }
        self.stats.lock().unwrap_or_else(|e| e.into_inner()).started = None;
        self.path.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Вызывается из видео-потока на каждый принятый кадр.
    fn submit(&self, jpeg: &[u8]) {
        let guard = self.tx.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(tx) = guard.as_ref() {
            match tx.try_send(jpeg.to_vec()) {
                Ok(()) => {}
                Err(std::sync::mpsc::TrySendError::Full(_)) => {
                    self.stats.lock().unwrap_or_else(|e| e.into_inner()).dropped += 1;
                }
                // писатель умер (диск) — фиксируем ошибку, если он не успел
                Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                    let mut s = self.stats.lock().unwrap_or_else(|e| e.into_inner());
                    if s.error.is_none() {
                        s.error = Some("поток записи остановился (диск?)".into());
                    }
                }
            }
        }
    }
}

/// Локальное время ГГГГММДД_ЧЧММСС без внешних зависимостей.
fn timestamp_local() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let (y, mth, d, h, m, s) = unsafe { local_datetime(secs) };
    format!("{y:04}{mth:02}{d:02}_{h:02}{m:02}{s:02}")
}

/// Локальная дата-время через CRT (только Windows-сборка UI).
#[cfg(target_os = "windows")]
unsafe fn local_datetime(unix: i64) -> (i32, i32, i32, i32, i32, i32) {
    #[repr(C)]
    struct Tm {
        tm_sec: i32, tm_min: i32, tm_hour: i32,
        tm_mday: i32, tm_mon: i32, tm_year: i32, tm_wday: i32, tm_yday: i32, tm_isdst: i32,
    }
    extern "system" {
        fn _localtime64(unix: *const i64) -> *mut Tm;
    }
    let tm = _localtime64(&unix);
    if tm.is_null() {
        return (1970, 1, 1, 0, 0, 0);
    }
    ((*tm).tm_year + 1900, (*tm).tm_mon + 1, (*tm).tm_mday,
     (*tm).tm_hour, (*tm).tm_min, (*tm).tm_sec)
}

#[cfg(not(target_os = "windows"))]
unsafe fn local_datetime(unix: i64) -> (i32, i32, i32, i32, i32, i32) {
    // UTC-раскладка (дней с эпохи → гражданская дата, Говард Хиннант)
    let days = unix.div_euclid(86_400);
    let rem = unix.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mth = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mth <= 2 { y + 1 } else { y };
    (y as i32, mth as i32, d as i32, (rem / 3600) as i32, ((rem % 3600) / 60) as i32, (rem % 60) as i32)
}


pub struct NetState {
    frame: Arc<Mutex<Option<VideoFrame>>>,
    frame_version: Arc<AtomicU64>,
    video_connected: Arc<AtomicBool>,
    control_connected: Arc<AtomicBool>,
    status: Arc<Mutex<Option<Status>>>,
    /// Когда пришёл последний статус (возраст данных для UI).
    status_at: Arc<Mutex<Option<Instant>>>,
    cmd_tx: Arc<Mutex<Option<std::sync::mpsc::Sender<UiCommand>>>>,
    video_fps: Arc<Mutex<(Instant, u32, f32)>>, // (окно, кадры, fps)
    rec: Arc<Recorder>,
}

impl NetState {
    pub fn new() -> Self {
        let s = Self {
            frame: Arc::new(Mutex::new(None)),
            frame_version: Arc::new(AtomicU64::new(0)),
            video_connected: Arc::new(AtomicBool::new(false)),
            control_connected: Arc::new(AtomicBool::new(false)),
            status: Arc::new(Mutex::new(None)),
            status_at: Arc::new(Mutex::new(None)),
            cmd_tx: Arc::new(Mutex::new(None)),
            video_fps: Arc::new(Mutex::new((Instant::now(), 0, 0.0))),
            rec: Arc::new(Recorder::new()),
        };
        spawn_video_listener(s.frame.clone(), s.frame_version.clone(), s.video_connected.clone(), s.video_fps.clone(), s.rec.clone());
        spawn_control_listener(
            s.control_connected.clone(),
            s.status.clone(),
            s.status_at.clone(),
            s.cmd_tx.clone(),
        );
        s
    }

    /// Запись стрима (кнопка REC в UI).
    pub fn recorder(&self) -> &Arc<Recorder> {
        &self.rec
    }

    pub fn take_video_frame(&self) -> Option<VideoFrame> {
        self.frame.lock().unwrap_or_else(|e| e.into_inner()).take()
    }

    /// Последний статус; None после обрыва канала (данные недостоверны —
    /// например, «НАВЕДЕНИЕ РАЗРЕШЕНО» не должно висеть над мёртвым каналом).
    pub fn status(&self) -> Option<Status> {
        self.status.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Возраст последнего статуса, с (для индикации «данные N с назад»).
    pub fn status_age_secs(&self) -> Option<f32> {
        self.status_at
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .map(|t| t.elapsed().as_secs_f32())
    }

    pub fn video_connected(&self) -> bool {
        self.video_connected.load(Ordering::Relaxed)
    }

    pub fn control_connected(&self) -> bool {
        self.control_connected.load(Ordering::Relaxed)
    }

    pub fn video_fps(&self) -> f32 {
        self.video_fps.lock().unwrap_or_else(|e| e.into_inner()).2
    }

    pub fn send(&self, cmd: UiCommand) {
        if let Some(tx) = self.cmd_tx.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            let _ = tx.send(cmd);
        }
    }
}

/// Видео: слушаем :9000, борд подключается и шлёт multipart M-JPEG.
/// Разбор — по JPEG-маркерам SOI/EOI (проверенный способ из viewer.py).
fn spawn_video_listener(
    slot: Arc<Mutex<Option<VideoFrame>>>,
    version: Arc<AtomicU64>,
    connected: Arc<AtomicBool>,
    fps_meter: Arc<Mutex<(Instant, u32, f32)>>,
    rec: Arc<Recorder>,
) {
    let _ = std::thread::Builder::new().name("video-rx".into()).spawn(move || {
        let listener = match TcpListener::bind("0.0.0.0:9000") {
            Ok(l) => l,
            Err(e) => {
                eprintln!("[VIDEO] не удалось слушать :9000 — {e}");
                return;
            }
        };
        loop {
            let (stream, _) = match listener.accept() {
                Ok(v) => v,
                Err(_) => continue,
            };
            connected.store(true, Ordering::Relaxed);
            eprintln!("[VIDEO] борт подключился");
            let mut reader = BufReader::new(stream);
            let mut buf = Vec::with_capacity(64 * 1024);
            let mut chunk = [0u8; 32 * 1024];
            'conn: loop {
                let n = match reader.read(&mut chunk) {
                    Ok(0) | Err(_) => break 'conn,
                    Ok(n) => n,
                };
                buf.extend_from_slice(&chunk[..n]);
                // достаём все полные JPEG из буфера
                while let Some((start, end)) = find_jpeg(&buf) {
                    let jpeg = buf[start..end].to_vec();
                    buf.drain(..end);
                    // запись ведётся из сырого JPEG до декодирования:
                    // перекодирования нет, файл = копия стрима
                    rec.submit(&jpeg);
                    match decode_rgba(&jpeg) {
                        Some((rgba, w, h)) => {
                            let v = version.fetch_add(1, Ordering::Relaxed) + 1;
                            *slot.lock().unwrap_or_else(|e| e.into_inner()) =
                                Some(VideoFrame { version: v, rgba, w, h });
                            let mut m = fps_meter.lock().unwrap_or_else(|e| e.into_inner());
                            m.1 += 1;
                            if m.0.elapsed() >= Duration::from_millis(500) {
                                m.2 = m.1 as f32 / m.0.elapsed().as_secs_f32();
                                m.0 = Instant::now();
                                m.1 = 0;
                            }
                        }
                        None => continue,
                    }
                }
                // защита от переполнения мусором
                if buf.len() > 8 * 1024 * 1024 {
                    buf.clear();
                }
            }
            connected.store(false, Ordering::Relaxed);
            eprintln!("[VIDEO] борт отключился, ждём переподключения");
        }
    });
}

/// Контроль: слушаем :9010, борд подключается; читаем статусы,
/// пишем команды (+ ping каждые 300 мс — keep-alive fail-safe борта).
fn spawn_control_listener(
    connected: Arc<AtomicBool>,
    status_slot: Arc<Mutex<Option<Status>>>,
    status_at: Arc<Mutex<Option<Instant>>>,
    cmd_slot: Arc<Mutex<Option<std::sync::mpsc::Sender<UiCommand>>>>,
) {
    let _ = std::thread::Builder::new().name("control".into()).spawn(move || {
        let listener = match TcpListener::bind("0.0.0.0:9010") {
            Ok(l) => l,
            Err(e) => {
                eprintln!("[CONTROL] не удалось слушать :9010 — {e}");
                return;
            }
        };
        loop {
            let (stream, _) = match listener.accept() {
                Ok(v) => v,
                Err(_) => continue,
            };
            connected.store(true, Ordering::Relaxed);
            eprintln!("[CONTROL] борт подключился");
            let read_sock = match stream.try_clone() {
                Ok(s) => s,
                Err(_) => {
                    connected.store(false, Ordering::Relaxed);
                    continue;
                }
            };
            let mut writer = stream;
            let (tx, rx) = std::sync::mpsc::channel::<UiCommand>();
            *cmd_slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx);
            // поток чтения статусов
            let st = status_slot.clone();
            let st_at = status_at.clone();
            let conn2 = connected.clone();
            let reader_handle = std::thread::spawn(move || {
                let mut reader = BufReader::new(read_sock);
                let mut line = String::new();
                loop {
                    line.clear();
                    match reader.read_line(&mut line) {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {
                            if let Ok(s) = serde_json::from_str::<Status>(line.trim()) {
                                *st.lock().unwrap_or_else(|e| e.into_inner()) = Some(s);
                                *st_at.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
                            }
                        }
                    }
                }
                // Обрыв канала: данные борта больше недостоверны (armed,
                // режим) — чистим, чтобы UI не показывал замороженное
                // состояние над мёртвым каналом.
                *st.lock().unwrap_or_else(|e| e.into_inner()) = None;
                *st_at.lock().unwrap_or_else(|e| e.into_inner()) = None;
                conn2.store(false, Ordering::Relaxed);
                eprintln!("[CONTROL] борт отключился");
            });
            // пишем команды и ping (с общим секретом канала, если задан)
            let token = std::env::var("SYNERGY_TOKEN").unwrap_or_default();
            let ping: Vec<u8> = if token.is_empty() {
                b"{\"t\":\"ping\"}\n".to_vec()
            } else {
                format!("{{\"t\":\"ping\",\"auth\":\"{token}\"}}\n").into_bytes()
            };
            let mut last_ping = Instant::now();
            loop {
                if !connected.load(Ordering::Relaxed) {
                    break;
                }
                // recv_timeout вместо sleep-поллинга: команда (LOCK после
                // двойного клика) уходит немедленно, а не с задержкой до
                // 20 мс; по таймауту — плановый ping.
                match rx.recv_timeout(Duration::from_millis(300)) {
                    Ok(cmd) => {
                        if writer.write_all(cmd.to_json(&token).as_bytes()).is_err() {
                            break;
                        }
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                }
                while let Ok(cmd) = rx.try_recv() {
                    if writer.write_all(cmd.to_json(&token).as_bytes()).is_err() {
                        break;
                    }
                }
                if last_ping.elapsed() >= Duration::from_millis(300) {
                    if writer.write_all(&ping).is_err() {
                        break;
                    }
                    last_ping = Instant::now();
                }
            }
            let _ = reader_handle.join();
            *cmd_slot.lock().unwrap_or_else(|e| e.into_inner()) = None;
            connected.store(false, Ordering::Relaxed);
        }
    });
}

/// Поиск полного JPEG (SOI..EOI) в буфере: (start, end).
fn find_jpeg(buf: &[u8]) -> Option<(usize, usize)> {
    let start = buf.windows(3).position(|w| w == b"\xff\xd8\xff")?;
    let end_marker = buf[start + 3..]
        .windows(2)
        .position(|w| w == b"\xff\xd9")?
        + start
        + 3
        + 2;
    Some((start, end_marker))
}

/// JPEG → RGBA (jpeg-decoder из workspace). Возвращает (пиксели, w, h).
fn decode_rgba(jpeg: &[u8]) -> Option<(Vec<u8>, usize, usize)> {
    let mut dec = jpeg_decoder::Decoder::new(jpeg);
    let pixels = dec.decode().ok()?;
    let info = dec.info()?;
    let (w, h) = (info.width as usize, info.height as usize);
    match info.pixel_format {
        jpeg_decoder::PixelFormat::RGB24 => Some((rgb_to_rgba(&pixels, w * h), w, h)),
        jpeg_decoder::PixelFormat::L8 => {
            let mut rgba = Vec::with_capacity(w * h * 4);
            for &p in &pixels {
                rgba.extend_from_slice(&[p, p, p, 255]);
            }
            Some((rgba, w, h))
        }
        _ => None,
    }
}

fn rgb_to_rgba(rgb: &[u8], px: usize) -> Vec<u8> {
    let mut rgba = Vec::with_capacity(px * 4);
    for p in rgb.chunks_exact(3) {
        rgba.extend_from_slice(&[p[0], p[1], p[2], 255]);
    }
    rgba
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_jpeg_bounds() {
        let mut buf = vec![0u8; 10];
        buf.extend_from_slice(b"\xff\xd8\xff\xe0DATA\xff\xd9");
        buf.extend_from_slice(&[0u8; 5]);
        let (s, e) = find_jpeg(&buf).unwrap();
        assert_eq!(&buf[s..s + 3], b"\xff\xd8\xff");
        assert_eq!(&buf[e - 2..e], b"\xff\xd9");
    }

    #[test]
    fn command_json_roundtrip() {
        let l = UiCommand::Lock { x: 320.0, y: 240.0, size: 100.0 }.to_json("");
        assert!(l.contains("\"t\":\"lock\""));
        let s = UiCommand::Stop.to_json("");
        assert_eq!(s.trim(), "{\"t\":\"stop\"}");
        let u = UiCommand::Unlock.to_json("");
        assert_eq!(u.trim(), "{\"t\":\"unlock\"}");
        // с общим секретом (safety §6.3) каждая команда несёт auth
        let a = UiCommand::Arm { on: true }.to_json("bench-secret");
        assert_eq!(a.trim(), "{\"t\":\"arm\",\"on\":true,\"auth\":\"bench-secret\"}");
        let p = UiCommand::Stop.to_json("tok");
        assert_eq!(p.trim(), "{\"t\":\"stop\",\"auth\":\"tok\"}");
    }

    #[test]
    fn recorder_writes_concat_of_jpegs() {
        let dir = std::env::temp_dir().join("synergy_rec_test");
        let rec = Recorder::new();
        let path = rec.start(&dir).expect("start");
        let a = b"\xff\xd8\xff\xe0AAAA\xff\xd9".to_vec();
        let b = b"\xff\xd8\xff\xe0BBBB\xff\xd9".to_vec();
        rec.submit(&a);
        rec.submit(&b);
        let stopped = rec.stop();
        assert_eq!(stopped.as_deref(), Some(path.as_path()));
        assert!(!rec.is_recording());
        // поток-писатель завершается асинхронно — даём ему момент
        for _ in 0..50 {
            if std::fs::metadata(&path).map(|m| m.len() >= (a.len() + b.len()) as u64).unwrap_or(false) {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let data = std::fs::read(&path).expect("файл записи");
        assert_eq!(data.len(), a.len() + b.len());
        assert_eq!(&data[..a.len()], &a[..]);
        assert_eq!(&data[a.len()..], &b[..]);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn timestamp_shape() {
        let t = timestamp_local();
        // ГГГГММДД_ЧЧММСС: 15 знаков с подчёркиванием на 9-й позиции
        assert_eq!(t.len(), 15, "{t}");
        assert_eq!(t.as_bytes()[8], b'_', "{t}");
        assert!(t.bytes().all(|c| c.is_ascii_digit() || c == b'_'));
    }
}

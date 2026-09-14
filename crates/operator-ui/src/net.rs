//! Сеть операторского приложения: приём видео-пуша (:9000) и
//! контрольного канала (:9010), команда — JSON-строки (ADR-016).

use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
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
    /// Готовые к ColorImage пиксели (конверсия сделана в net-потоке).
    pub rgba: Vec<egui::Color32>,
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

/// Запись стрима в AVI (M-JPEG): приёмный поток отдаёт сырые JPEG в канал,
/// поток-писатель складывает их в AVI-контейнер с fourcc MJPG.
///
/// Почему контейнер, а не конкатенация JPEG: сырая склейка открывается
/// плеерами как ОДИН кадр (VLC/«Фотографии» декодируют первый JPEG и
/// останавливаются) — на борде это выглядело как «записался только первый
/// кадр», хотя данные в файле были все. AVI/MJPG играется целиком в VLC,
/// «Кино и ТВ»/MPC и при этом остаётся replay-совместимым: replay борта
/// и тесты сканируют SOI/EOI-маркеры, межкадровые заголовки контейнера
/// они просто пропускают.
///
/// Долговечность: писатель флашит буфер раз в секунду; stop() дописывает
/// индекс кадров и патчит размеры в заголовке (джойн) — закрытие окна не
/// теряет хвост.
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

    /// Начать запись в файл `dir/synergy_ГГГГММДД_ЧЧММСС.avi`.
    pub fn start(&self, dir: &std::path::Path) -> Result<PathBuf, String> {
        if self.is_recording() {
            return Err("запись уже идёт".into());
        }
        std::fs::create_dir_all(dir).map_err(|e| format!("каталог записи: {e}"))?;
        let path = dir.join(format!("synergy_{}.avi", timestamp_local()));
        let file = std::fs::File::create(&path).map_err(|e| format!("файл записи: {e}"))?;
        let (tx, rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(240);
        let stats = self.stats.clone();
        *stats.lock().unwrap_or_else(|e| e.into_inner()) =
            RecStats { started: Some(Instant::now()), ..Default::default() };
        let handle = std::thread::Builder::new()
            .name("rec-writer".into())
            .spawn(move || avi_writer_loop(file, rx, stats))
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

/// Частота кадров в заголовке AVI: борт пушит 60/N fps (по умолчанию
/// frame_div=2 → 30); тайминги воспроизведения, не данные.
const AVI_FPS: u32 = 30;

/// Оффсеты полей заголовка, которые патчатся при финализации
/// (размеры известны только после последнего кадра).
struct AviPatches {
    riff_size: usize,
    /// avih.dwFlags: 0 во время записи, AVIF_HASINDEX при финализации.
    avih_flags: usize,
    avih_frames: usize,
    strh_length: usize,
    movi_size: usize,
    /// Позиция fourcc 'movi' — база оффсетов idx1 и начало содержимого LIST.
    movi_fourcc: usize,
}

fn put_u32(b: &mut Vec<u8>, v: u32) {
    b.extend_from_slice(&v.to_le_bytes());
}

fn put_u16(b: &mut Vec<u8>, v: u16) {
    b.extend_from_slice(&v.to_le_bytes());
}

fn put_fourcc(b: &mut Vec<u8>, s: &[u8; 4]) {
    b.extend_from_slice(s);
}

/// Заголовок AVI (один видеопоток MJPG) с нулевыми размерами-заглушками.
fn avi_header(w: u32, h: u32) -> (Vec<u8>, AviPatches) {
    let mut b = Vec::with_capacity(600);
    put_fourcc(&mut b, b"RIFF");
    let riff_size = b.len();
    put_u32(&mut b, 0);
    put_fourcc(&mut b, b"AVI ");

    let strl_list = 4 + (8 + 56) + (8 + 40); // 'strl' + strh + strf
    put_fourcc(&mut b, b"LIST");
    put_u32(&mut b, 4 + (8 + 56) + (8 + strl_list)); // 'hdrl' + avih + strl
    put_fourcc(&mut b, b"hdrl");

    put_fourcc(&mut b, b"avih");
    put_u32(&mut b, 56);
    put_u32(&mut b, 1_000_000 / AVI_FPS.max(1)); // usec на кадр
    put_u32(&mut b, 4_000_000); // max байт/с (примерно)
    put_u32(&mut b, 0); // padding granularity
    let avih_flags = b.len();
    // AVIF_HASINDEX ставится ТОЛЬКО при финализации (когда idx1 дописан);
    // при жёстком убийстве процесса файла без индекса, но с честными
    // размерами достаточно плеерам (VLC сканирует чанки 'movi')
    put_u32(&mut b, 0);
    let avih_frames = b.len();
    put_u32(&mut b, 0); // всего кадров — патч при финализации
    put_u32(&mut b, 0); // initial frames
    put_u32(&mut b, 1); // потоков
    put_u32(&mut b, 1 << 20); // рекомендуемый буфер
    put_u32(&mut b, w);
    put_u32(&mut b, h);
    for _ in 0..4 {
        put_u32(&mut b, 0);
    }

    put_fourcc(&mut b, b"LIST");
    put_u32(&mut b, strl_list);
    put_fourcc(&mut b, b"strl");
    put_fourcc(&mut b, b"strh");
    put_u32(&mut b, 56);
    put_fourcc(&mut b, b"vids");
    put_fourcc(&mut b, b"MJPG");
    put_u32(&mut b, 0); // flags
    put_u16(&mut b, 0); // priority
    put_u16(&mut b, 0); // language
    put_u32(&mut b, 0); // initial frames
    put_u32(&mut b, 1); // scale
    put_u32(&mut b, AVI_FPS); // rate = fps при scale=1
    put_u32(&mut b, 0); // start
    let strh_length = b.len();
    put_u32(&mut b, 0); // кадров — патч
    put_u32(&mut b, 1 << 20); // рекомендуемый буфер
    put_u32(&mut b, 0xFFFF_FFFF); // качество
    put_u32(&mut b, 0); // размер сэмпла (переменный)
    put_u16(&mut b, 0); // rcFrame left
    put_u16(&mut b, 0); // top
    put_u16(&mut b, w as u16); // right
    put_u16(&mut b, h as u16); // bottom

    put_fourcc(&mut b, b"strf");
    put_u32(&mut b, 40); // BITMAPINFOHEADER
    put_u32(&mut b, 40); // biSize
    put_u32(&mut b, w);
    put_u32(&mut b, h);
    put_u16(&mut b, 1); // плоскости
    put_u16(&mut b, 24); // бит на пиксель
    put_fourcc(&mut b, b"MJPG");
    put_u32(&mut b, w.saturating_mul(h).saturating_mul(3));
    for _ in 0..4 {
        put_u32(&mut b, 0); // ppm/использованные/важные цвета
    }

    put_fourcc(&mut b, b"LIST");
    let movi_size = b.len();
    put_u32(&mut b, 0); // размер 'movi'-листа — патч
    let movi_fourcc = b.len();
    put_fourcc(&mut b, b"movi");
    (
        b,
        AviPatches {
            riff_size,
            avih_flags,
            avih_frames,
            strh_length,
            movi_size,
            movi_fourcc,
        },
    )
}

/// Размеры JPEG из маркера SOF (нужны заголовку AVI до первого кадра).
/// Нет SOF (экзотика) — 640×480, как дефолт пульта.
fn jpeg_dims(jpeg: &[u8]) -> Option<(u32, u32)> {
    let mut i = 2usize; // после SOI
    while i + 9 < jpeg.len() {
        if jpeg[i] != 0xFF {
            return None;
        }
        match jpeg[i + 1] {
            0xC0..=0xC3 => {
                let h = u16::from_be_bytes([jpeg[i + 5], jpeg[i + 6]]) as u32;
                let w = u16::from_be_bytes([jpeg[i + 7], jpeg[i + 8]]) as u32;
                return Some((w, h));
            }
            _ => {
                let len = u16::from_be_bytes([jpeg[i + 2], jpeg[i + 3]]) as usize;
                i += 2 + len;
            }
        }
    }
    None
}

/// Цикл писателя: JPEG → чанки '00dc' внутри 'movi'; по завершении —
/// индекс idx1 и патч размеров заголовка.
///
/// Стойкость к жёсткому убийству процесса (kill -9 / TerminateProcess):
/// каждые 2 с размеры RIFF/movi и счётчики кадров патчатся по месту,
/// так что не финализированный файл остаётся валидным AVI без индекса —
/// плееры играют его до последнего патча.
///
/// Патчи идут через ОДИН дескриптор (BufWriter::get_mut после flush с
/// возвратом позиции в конец): клон File (try_clone/dup) ДЕЛИТ указатель
/// позиции с оригиналом — seek патча молча сдвигал общую позицию, и
/// следующий write перезаписывал файл с начала.
fn avi_writer_loop(
    file: std::fs::File,
    rx: std::sync::mpsc::Receiver<Vec<u8>>,
    stats: Arc<Mutex<RecStats>>,
) {
    let mut w = std::io::BufWriter::with_capacity(4 * 1024 * 1024, file);
    let mut patches: Option<AviPatches> = None;
    // (оффсет чанка от 'movi', размер JPEG без паддинга)
    let mut idx: Vec<(u32, u32)> = Vec::new();
    let mut chunk_off: u32 = 4; // первый чанк сразу за fourcc 'movi'
    let mut frames: u32 = 0;
    let mut last_patch = Instant::now();
    loop {
        match rx.recv_timeout(Duration::from_secs(1)) {
            Ok(jpeg) => {
                if patches.is_none() {
                    let (w_, h_) = jpeg_dims(&jpeg).unwrap_or((640, 480));
                    let (hdr, p) = avi_header(w_, h_);
                    if let Err(e) = w.write_all(&hdr) {
                        let mut s = stats.lock().unwrap_or_else(|e| e.into_inner());
                        s.error = Some(format!("запись на диск: {e}"));
                        break;
                    }
                    patches = Some(p);
                }
                // чанк: fourcc + размер + данные (+ байт выравнивания до чётного)
                let padded = jpeg.len() + jpeg.len() % 2;
                let mut head = [0u8; 8];
                head[..4].copy_from_slice(b"00dc");
                head[4..].copy_from_slice(&(jpeg.len() as u32).to_le_bytes());
                let err = w.write_all(&head)
                    .and_then(|_| w.write_all(&jpeg))
                    .and_then(|_| {
                        if padded > jpeg.len() {
                            w.write_all(&[0u8])
                        } else {
                            Ok(())
                        }
                    });
                if let Err(e) = err {
                    let mut s = stats.lock().unwrap_or_else(|e| e.into_inner());
                    s.error = Some(format!("запись на диск: {e}"));
                    break;
                }
                idx.push((chunk_off, jpeg.len() as u32));
                chunk_off += 8 + padded as u32;
                frames += 1;
                let mut s = stats.lock().unwrap_or_else(|e| e.into_inner());
                s.frames += 1;
                s.bytes += jpeg.len() as u64;
                // периодический патч: файл валиден и без финализации
                if last_patch.elapsed() >= Duration::from_secs(2) {
                    last_patch = Instant::now();
                    let _ = w.flush();
                    if let Some(p) = patches.as_ref() {
                        patch_open_sizes(w.get_mut(), p, frames, chunk_off);
                    }
                }
            }
            // таймаут: канал пуст ≥1 с — флашим, чтобы хвост
            // не копился в буфере (durability при закрытии окна)
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                let _ = w.flush();
            }
            // stop() разорвал канал — финализируем контейнер
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    let _ = w.flush();
    // Кадров не было — всё равно валидный пустой AVI (нулевая длительность).
    if patches.is_none() {
        let (hdr, p) = avi_header(640, 480);
        let _ = w.write_all(&hdr);
        patches = Some(p);
    }
    let Ok(mut file) = w.into_inner() else {
        let mut s = stats.lock().unwrap_or_else(|e| e.into_inner());
        s.error.get_or_insert_with(|| "финализация записи: буфер не сброшен".into());
        return;
    };
    let p = patches.expect("заголовок записан выше");
    let idx1_pos = file.stream_position().unwrap_or(0);
    // idx1: по 16 байт на кадр (fourcc, флаг keyframe, оффсет от 'movi', размер)
    let mut tail = Vec::with_capacity(8 + idx.len() * 16);
    put_fourcc(&mut tail, b"idx1");
    put_u32(&mut tail, (idx.len() * 16) as u32);
    for (off, size) in idx {
        put_fourcc(&mut tail, b"00dc");
        put_u32(&mut tail, 0x10); // AVIIF_KEYFRAME — каждый JPEG самодостаточен
        put_u32(&mut tail, off);
        put_u32(&mut tail, size);
    }
    if file.write_all(&tail).is_err() {
        let mut s = stats.lock().unwrap_or_else(|e| e.into_inner());
        s.error.get_or_insert_with(|| "финализация записи: индекс".into());
    }
    let total = file.stream_position().unwrap_or(idx1_pos + tail.len() as u64);
    // размеры-заглушки → реальные значения
    let mut patch = |at: usize, v: u32| {
        let _ = file.seek(SeekFrom::Start(at as u64));
        let _ = file.write_all(&v.to_le_bytes());
    };
    patch(p.riff_size, (total - 8) as u32);
    patch(p.avih_flags, 0x10); // теперь индекс есть — AVIF_HASINDEX
    patch(p.avih_frames, frames);
    patch(p.strh_length, frames);
    patch(p.movi_size, (idx1_pos - p.movi_fourcc as u64) as u32);
    let _ = file.flush();
}

/// Патч размеров «открытого» (ещё пишущегося) AVI — файл после жёсткого
/// убийства процесса остаётся валидным (без idx1, флаг HASINDEX не ставится).
/// `movi_bytes` — содержимое LIST 'movi' на момент патча (== chunk_off).
/// Позиция дескриптора возвращается в конец — писатель продолжает аппендить.
fn patch_open_sizes(f: &mut std::fs::File, p: &AviPatches, frames: u32, movi_bytes: u32) {
    let riff = (p.movi_fourcc as u64 + movi_bytes as u64 - 8) as u32;
    let mut at = |off: usize, v: u32| {
        let _ = f.seek(SeekFrom::Start(off as u64));
        let _ = f.write_all(&v.to_le_bytes());
    };
    at(p.riff_size, riff);
    at(p.avih_frames, frames);
    at(p.strh_length, frames);
    at(p.movi_size, movi_bytes);
    let _ = f.seek(SeekFrom::End(0));
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
    /// Когда из стрима достанут последний полный JPEG (свежесть сигнала:
    /// TCP может «жить», а кадры не приходить — тогда запись пуста).
    last_frame_at: Arc<Mutex<Option<Instant>>>,
    /// Ошибки bind :9000/:9010 (порт занят второй копией пульта и т.п.).
    /// Раньше уходили только в eprintln — в оконном приложении невидимо.
    bind_errors: Arc<Mutex<Vec<String>>>,
    rec: Arc<Recorder>,
}

impl NetState {
    pub fn new(repaint: Option<egui::Context>) -> Self {
        let s = Self {
            frame: Arc::new(Mutex::new(None)),
            frame_version: Arc::new(AtomicU64::new(0)),
            video_connected: Arc::new(AtomicBool::new(false)),
            control_connected: Arc::new(AtomicBool::new(false)),
            status: Arc::new(Mutex::new(None)),
            status_at: Arc::new(Mutex::new(None)),
            cmd_tx: Arc::new(Mutex::new(None)),
            video_fps: Arc::new(Mutex::new((Instant::now(), 0, 0.0))),
            last_frame_at: Arc::new(Mutex::new(None)),
            bind_errors: Arc::new(Mutex::new(Vec::new())),
            rec: Arc::new(Recorder::new()),
        };
        spawn_video_listener(
            s.frame.clone(),
            s.frame_version.clone(),
            s.video_connected.clone(),
            s.video_fps.clone(),
            s.last_frame_at.clone(),
            s.bind_errors.clone(),
            s.rec.clone(),
            repaint,
        );
        spawn_control_listener(
            s.control_connected.clone(),
            s.status.clone(),
            s.status_at.clone(),
            s.cmd_tx.clone(),
            s.bind_errors.clone(),
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

    /// Возраст последнего кадра стрима, с (None = кадров ещё не было).
    pub fn frame_age_secs(&self) -> Option<f32> {
        self.last_frame_at
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .map(|t| t.elapsed().as_secs_f32())
    }

    /// Ошибки bind портов (порт занят — например, вторая копия пульта).
    pub fn bind_errors(&self) -> Vec<String> {
        self.bind_errors.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn send(&self, cmd: UiCommand) {
        if let Some(tx) = self.cmd_tx.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            let _ = tx.send(cmd);
        }
    }
}

/// Видео: слушаем :9000, борд подключается и шлёт multipart M-JPEG.
/// Разбор — по JPEG-маркерам SOI/EOI (проверенный способ из viewer.py).
#[allow(clippy::too_many_arguments)] // разделяемые слоты состояния — их 8
fn spawn_video_listener(
    slot: Arc<Mutex<Option<VideoFrame>>>,
    version: Arc<AtomicU64>,
    connected: Arc<AtomicBool>,
    fps_meter: Arc<Mutex<(Instant, u32, f32)>>,
    last_frame_at: Arc<Mutex<Option<Instant>>>,
    bind_errors: Arc<Mutex<Vec<String>>>,
    rec: Arc<Recorder>,
    repaint: Option<egui::Context>,
) {
    let _ = std::thread::Builder::new().name("video-rx".into()).spawn(move || {
        let listener = match TcpListener::bind("0.0.0.0:9000") {
            Ok(l) => l,
            Err(e) => {
                eprintln!("[VIDEO] не удалось слушать :9000 — {e}");
                bind_errors
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(format!(
                        "порт 9000 (видео) занят — {e}; вероятно, запущена вторая копия пульта"
                    ));
                return;
            }
        };
        loop {
            let (stream, _) = match listener.accept() {
                Ok(v) => v,
                Err(_) => continue,
            };
            connected.store(true, Ordering::Relaxed);
            // точка отсчёта свежести: даём 2 с на первые кадры,
            // дальше «НЕТ СИГНАЛА» честно скажет, что кадры не идут
            *last_frame_at.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
            eprintln!("[VIDEO] борт подключился");
            let mut reader = BufReader::new(stream);
            let mut buf = Vec::with_capacity(64 * 1024);
            let mut chunk = [0u8; 32 * 1024];
            // Инкрементальный скан: неполный кадр не пересканируется с
            // нуля на каждый чанк (при 30-60 fps это 3-6× амплификация
            // байтового сканирования), а с последней позиции (−3 на
            // straddle маркера).
            let mut scan_from = 0usize;
            'conn: loop {
                let n = match reader.read(&mut chunk) {
                    Ok(0) | Err(_) => break 'conn,
                    Ok(n) => n,
                };
                buf.extend_from_slice(&chunk[..n]);
                // достаём все полные JPEG из буфера
                loop {
                    let Some(rel) = find_soi(&buf[scan_from..]) else {
                        scan_from = buf.len().saturating_sub(3);
                        break;
                    };
                    let start = scan_from + rel;
                    let Some(rel_end) = find_eoi(&buf[start + 3..]) else {
                        scan_from = start; // кадр копится — ждём хвост
                        break;
                    };
                    let end = start + 3 + rel_end + 2;
                    let jpeg = buf[start..end].to_vec();
                    buf.drain(..end);
                    scan_from = 0;
                    // запись ведётся из сырого JPEG до декодирования:
                    // перекодирования нет, файл = копия стрима
                    *last_frame_at.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
                    rec.submit(&jpeg);
                    match decode_rgba(&jpeg) {
                        Some((rgba, w, h)) => {
                            let v = version.fetch_add(1, Ordering::Relaxed) + 1;
                            *slot.lock().unwrap_or_else(|e| e.into_inner()) =
                                Some(VideoFrame { version: v, rgba, w, h });
                            // Событийная перерисовка: кадр пришёл — UI
                            // обновится сразу, таймер 33 мс остаётся
                            // фолбэком для статусов без видео.
                            if let Some(ctx) = repaint.as_ref() {
                                ctx.request_repaint();
                            }
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
    bind_errors: Arc<Mutex<Vec<String>>>,
) {
    let _ = std::thread::Builder::new().name("control".into()).spawn(move || {
        let listener = match TcpListener::bind("0.0.0.0:9010") {
            Ok(l) => l,
            Err(e) => {
                eprintln!("[CONTROL] не удалось слушать :9010 — {e}");
                bind_errors
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(format!(
                        "порт 9010 (управление) занят — {e}; вероятно, запущена вторая копия пульта"
                    ));
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
#[cfg(test)]
fn find_jpeg(buf: &[u8]) -> Option<(usize, usize)> {
    let start = find_soi(buf)?;
    let end = find_eoi(&buf[start + 3..])? + start + 3 + 2;
    Some((start, end))
}

fn find_soi(buf: &[u8]) -> Option<usize> {
    buf.windows(3).position(|w| w == b"\xff\xd8\xff")
}

fn find_eoi(buf: &[u8]) -> Option<usize> {
    buf.windows(2).position(|w| w == b"\xff\xd9")
}

/// JPEG → Color32 (zune-jpeg: ~2-4× быстрее чистого Rust-декодера).
/// Конверсия RGB→Color32 одна, без промежуточного RGBA-буфера.
fn decode_rgba(jpeg: &[u8]) -> Option<(Vec<egui::Color32>, usize, usize)> {
    use zune_jpeg::zune_core::colorspace::ColorSpace;
    let mut dec = zune_jpeg::JpegDecoder::new(jpeg);
    let pixels = dec.decode().ok()?;
    let (w, h) = dec.dimensions()?;
    let out = match dec.get_output_colorspace()? {
        ColorSpace::RGB => {
            let mut px = Vec::with_capacity(w * h);
            for p in pixels.chunks_exact(3) {
                px.push(egui::Color32::from_rgb(p[0], p[1], p[2]));
            }
            px
        }
        ColorSpace::Luma => {
            let mut px = Vec::with_capacity(w * h);
            for &g in &pixels {
                px.push(egui::Color32::from_gray(g));
            }
            px
        }
        _ => return None,
    };
    Some((out, w, h))
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
    fn recorder_writes_avi_container() {
        let dir = std::env::temp_dir().join("synergy_rec_test");
        let rec = Recorder::new();
        let path = rec.start(&dir).expect("start");
        let a = b"\xff\xd8\xff\xe0AAAA\xff\xd9".to_vec();
        let b = b"\xff\xd8\xff\xe0BBBB\xff\xd9".to_vec();
        rec.submit(&a);
        rec.submit(&b);
        // stop() джойнит писателя — контейнер финализирован (индекс, размеры)
        let stopped = rec.stop();
        assert_eq!(stopped.as_deref(), Some(path.as_path()));
        assert!(!rec.is_recording());
        let data = std::fs::read(&path).expect("файл записи");
        // валидный контейнер: RIFF/AVI, MJPG, индекс кадров, честный размер
        assert_eq!(&data[0..4], b"RIFF", "нет RIFF-магики");
        assert_eq!(&data[8..12], b"AVI ", "нет AVI-типа");
        assert!(data.windows(4).any(|w| w == b"MJPG"), "нет fourcc MJPG");
        let riff = u32::from_le_bytes([data[4], data[5], data[6], data[7]]) as usize;
        assert_eq!(riff + 8, data.len(), "размер RIFF != размеру файла");
        let idx_at = data.windows(4).position(|w| w == b"idx1").expect("нет idx1");
        let idx_size =
            u32::from_le_bytes(data[idx_at + 4..idx_at + 8].try_into().unwrap());
        assert_eq!(idx_size / 16, 2, "в индексе должно быть 2 кадра");
        // replay-совместимость (скан SOI/EOI как у борта): оба JPEG достаются
        assert_eq!(data.windows(3).filter(|w| *w == b"\xff\xd8\xff").count(), 2);
        assert!(data.windows(a.len()).any(|w| w == a.as_slice()));
        assert!(data.windows(b.len()).any(|w| w == b.as_slice()));
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

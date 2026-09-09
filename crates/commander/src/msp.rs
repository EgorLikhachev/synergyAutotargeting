//! MSP v1 (Betaflight/INAV) — кодек фреймов для управления подвесом/полётом.
//! Порт wire-формата MSP (фаза D, ADR-012):
//!
//! `$M<` + `<len:u8>` + `<cmd:u8>` + `<payload LE>` + `<crc:u8>`,
//! crc = len ^ cmd ^ все байты payload (XOR).
//!
//! Основное сообщение — SET_RAW_RC (200): 16 RC-каналов, u16 LE, 1000..2000 мкс.

/// MSP_SET_RAW_RC (главный канал управления).
pub const MSP_SET_RAW_RC: u8 = 200;
/// MSP_STATUS — флаги состояния полётника (в т.ч. armingDisableFlags).
pub const MSP_STATUS: u8 = 101;
/// MSP_RC — эхо RC-каналов, как их видит полётник.
pub const MSP_RC: u8 = 105;
/// MSP_RAW_GPS (запрос телеметрии GPS у полётника).
pub const MSP_RAW_GPS: u8 = 106;

/// Бит armingDisableFlags, определён ЭМПИРИЧЕСКИ на GEPRCF405 / BF 4.4.3
/// (2026-09-08, дампы в ADR-021): установлен ⟺ полётник видит живой
/// RC-поток с aux-arm (наши кадры несут aux1=1950 постоянно). При тишине
/// >0,5 с бит пропадает вместе с RC (failsafe). Используется как индикатор
/// > «FC видит RC» в телеметрии пульта. Постоянный шум других бит
/// > (0x0200000c на этом стенде) игнорируется.
pub const ARMING_FLAG_RC_ALIVE: u32 = 1 << 7;

/// Телеметрия FC, публикуемая в статус UI (ADR-021).
#[derive(Debug, Clone, Default)]
pub struct FcTelemetry {
    /// Был хоть один MSP-ответ (линк борт↔FC жив).
    pub online: bool,
    /// Полётник видит живой RC-поток (ARMING_FLAG_RC_ALIVE).
    pub rx_ok: bool,
    /// Полные armingDisableFlags (диагностика).
    pub flags: u32,
    /// Эхо RC-каналов из MSP_RC (мкс, как их видит FC).
    pub ch: Vec<u16>,
}

/// CRC8-XOR полезной нагрузки MSP v1.
pub fn crc8(len: u8, cmd: u8, payload: &[u8]) -> u8 {
    let mut c = len ^ cmd;
    for b in payload {
        c ^= b;
    }
    c
}

/// Собрать фрейм `$M<` для команды с payload.
pub fn frame(cmd: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 6);
    out.extend_from_slice(b"$M<");
    out.push(payload.len() as u8);
    out.push(cmd);
    out.extend_from_slice(payload);
    out.push(crc8(payload.len() as u8, cmd, payload));
    out
}

/// RC-каналы по умолчанию: стики в центре, aux в минимум.
pub fn center_channels() -> [u16; 16] {
    [1500; 16]
}

/// SET_RAW_RC: 16 каналов (мкс, LE).
pub fn set_raw_rc(ch: &[u16; 16]) -> Vec<u8> {
    let mut payload = Vec::with_capacity(32);
    for &v in ch {
        payload.extend_from_slice(&v.to_le_bytes());
    }
    frame(MSP_SET_RAW_RC, &payload)
}

/// MSP2_SET_ARMING (0x0323) — дословный порт `arm_packet` предшественника:
/// `$M< 03 23 03 <01|00> crc`, crc = XOR всех байт фрейма (включая заголовок
/// и длину — особенность референсной реализации, отличается от v1-правила).
///
/// **ТОЛЬКО ручной/тестовый API.** Запрещено вызывать из рабочих контуров
/// (fail-safe, авто-разарм): платформа летающая, disarm в воздухе =
/// отключение моторов = падение (решение заказчика 2026-09-05). Рабочий
/// «разарм» — центры стиков (`law.lost()`) + молчание RC; поведение моторов
/// при молчании задаёт failsafe Betaflight.
pub fn set_arming(arm: bool) -> Vec<u8> {
    let mut out = vec![0x24, 0x4D, 0x3C, 0x03, 0x23, 0x03, u8::from(arm)];
    let crc = out.iter().fold(0u8, |a, &b| a ^ b);
    out.push(crc);
    out
}

/// Запрос MSP_RAW_GPS (без payload).
pub fn raw_gps_request() -> Vec<u8> {
    frame(MSP_RAW_GPS, &[])
}

/// Запрос MSP_STATUS (без payload).
pub fn status_request() -> Vec<u8> {
    frame(MSP_STATUS, &[])
}

/// Запрос MSP_RC (без payload).
pub fn rc_request() -> Vec<u8> {
    frame(MSP_RC, &[])
}

/// Инкрементальный разбор потока ответов `$M>`: кормим чанками (read из
/// порта), на выходе — полные кадры `(cmd, payload)` с проверенной CRC.
/// При повреждённом кадре — ресинхронизация поиском следующего заголовка.
#[derive(Default)]
pub struct ReplyParser {
    buf: Vec<u8>,
}

impl ReplyParser {
    pub fn new() -> Self {
        Self { buf: Vec::with_capacity(256) }
    }

    pub fn feed(&mut self, chunk: &[u8]) -> Vec<(u8, Vec<u8>)> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        loop {
            // ищем заголовок $M>
            let mut start = None;
            for i in 0..self.buf.len().saturating_sub(2) {
                if &self.buf[i..i + 3] == b"$M>" {
                    start = Some(i);
                    break;
                }
            }
            let Some(start) = start else {
                self.buf.clear();
                return out;
            };
            if start > 0 {
                self.buf.drain(..start);
            }
            if self.buf.len() < 5 {
                return out; // ждём шапку: $M> len cmd
            }
            let len = self.buf[3] as usize;
            let total = 5 + len + 1; // шапка + payload + crc
            if self.buf.len() < total {
                return out; // ждём хвост кадра
            }
            let cmd = self.buf[4];
            let payload = self.buf[5..5 + len].to_vec();
            let crc = self.buf[5 + len];
            if crc8(len as u8, cmd, &payload) == crc {
                out.push((cmd, payload));
                self.buf.drain(..total);
            } else {
                // битый кадр: пропускаем заголовок, ищем следующий
                self.buf.drain(..3);
            }
        }
    }
}

/// armingDisableFlags из ответа MSP_STATUS (BF 4.4): хвост payload —
/// `... [count:u8] [flags:u32] [config:u8]`.
pub fn parse_status_flags(payload: &[u8]) -> Option<u32> {
    if payload.len() < 6 {
        return None;
    }
    let n = payload.len();
    Some(u32::from_le_bytes(payload[n - 5..n - 1].try_into().ok()?))
}

/// Каналы из ответа MSP_RC (пары u16 LE).
pub fn parse_rc(payload: &[u8]) -> Vec<u16> {
    payload
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect()
}

/// Разбор ответа `$M>` на raw_gps_request: (fix, sat, lat, lon, alt_m, speed_kmh).
pub fn parse_raw_gps(data: &[u8]) -> Option<(u8, u8, f64, f64, i16, f32)> {
    // $M> len cmd payload crc
    if data.len() < 7 || &data[..3] != b"$M>" {
        return None;
    }
    let len = data[3] as usize;
    let cmd = data[4];
    let payload = data.get(5..5 + len)?;
    if cmd != MSP_RAW_GPS || payload.len() < 16 {
        return None;
    }
    let fix = payload[0];
    let sat = payload[1];
    let lat = i32::from_le_bytes(payload[2..6].try_into().ok()?) as f64 / 1e7;
    let lon = i32::from_le_bytes(payload[6..10].try_into().ok()?) as f64 / 1e7;
    let alt = i16::from_le_bytes(payload[10..12].try_into().ok()?);
    let speed_cms = u16::from_le_bytes(payload[12..14].try_into().ok()?);
    Some((fix, sat, lat, lon, alt, speed_cms as f32 * 0.036))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_matches_msp_reference() {
        // crc = len ^ cmd ^ payload...
        let payload = [1u8, 2, 3];
        assert_eq!(crc8(3, 200, &payload), 3 ^ 200 ^ 1 ^ 2 ^ 3);
    }

    #[test]
    fn set_raw_rc_frame_is_byte_exact() {
        let mut ch = center_channels();
        ch[0] = 1600;
        ch[3] = 1200;
        let f = set_raw_rc(&ch);
        assert_eq!(&f[..3], b"$M<");
        assert_eq!(f[3], 32); // длина
        assert_eq!(f[4], 200); // SET_RAW_RC
        assert_eq!(f.len(), 3 + 1 + 1 + 32 + 1);
        // Первый канал LE по смещению 5
        assert_eq!(u16::from_le_bytes([f[5], f[6]]), 1600);
        // CRC пересчитан вручную
        let expect = {
            let mut c = 32u8 ^ 200u8;
            for b in &f[5..37] {
                c ^= b;
            }
            c
        };
        assert_eq!(f[37], expect);
    }

    #[test]
    fn arming_frame_shape() {
        let f = set_arming(true);
        assert_eq!(&f[..3], b"$M<");
        assert_eq!(f[3], 3);
        // данные команды MSP2_SET_ARMING: [$M<][03][23][03][01][crc]
        assert_eq!(f[4], 0x23);
        assert_eq!(f[5], 0x03);
        assert_eq!(f[6], 1);
    }

    #[test]
    fn gps_roundtrip() {
        let mut resp = Vec::new();
        resp.extend_from_slice(b"$M>");
        resp.push(16);
        resp.push(MSP_RAW_GPS);
        resp.extend_from_slice(&[2, 12]); // fix 3D, 12 спутников
        resp.extend_from_slice(&(557_558_430i32).to_le_bytes()); // lat
        resp.extend_from_slice(&(376_176_980i32).to_le_bytes()); // lon
        resp.extend_from_slice(&(120i16).to_le_bytes());
        resp.extend_from_slice(&(500u16).to_le_bytes());
        resp.extend_from_slice(&(1800u16).to_le_bytes()); // ground course
        resp.push(0); // crc-байт (не проверяем в parse)
        let (fix, sat, lat, _, alt, _) = parse_raw_gps(&resp).unwrap();
        assert_eq!((fix, sat), (2, 12));
        assert!((lat - 55.755843).abs() < 1e-5);
        assert_eq!(alt, 120);
    }

    #[test]
    fn reply_parser_handles_split_and_garbage() {
        let mk = |cmd: u8, payload: &[u8]| -> Vec<u8> {
            let mut v = b"$M>".to_vec();
            v.push(payload.len() as u8);
            v.push(cmd);
            v.extend_from_slice(payload);
            v.push(crc8(payload.len() as u8, cmd, payload));
            v
        };
        let mut p = ReplyParser::new();
        // два кадра + мусор спереди, поданные кривыми чанками
        let mut stream = Vec::new();
        stream.extend_from_slice(b"\x00garb");
        stream.extend_from_slice(&mk(MSP_STATUS, &[0u8; 22]));
        stream.extend_from_slice(&mk(MSP_RC, &[0xdc, 0x05]));
        let chunks: Vec<&[u8]> = vec![&stream[..3], &stream[3..11], &stream[11..]];
        for c in chunks {
            for (cmd, payload) in p.feed(c) {
                if cmd == MSP_STATUS {
                    assert_eq!(payload.len(), 22);
                } else if cmd == MSP_RC {
                    assert_eq!(parse_rc(&payload), vec![1500]);
                }
            }
        }
        // кадр с битой CRC выбрасывается, следующий читается
        let mut bad = mk(MSP_RC, &[0x10, 0x03]);
        let last = bad.len() - 1;
        bad[last] ^= 0xFF;
        let mut s2 = bad;
        s2.extend_from_slice(&mk(MSP_RC, &[0x1e, 0x05]));
        let got: Vec<u8> = p.feed(&s2).into_iter().map(|(c, _)| c).collect();
        assert_eq!(got, vec![MSP_RC]);
    }

    #[test]
    fn status_flags_from_real_dump() {
        // Реальный дамп GEPRCF405/BF4.4.3 (ADR-021): payload MSP_STATUS,
        // хвост ... count=0x1a, flags, config
        let payload: Vec<u8> = vec![
            0x7b, 0x00, 0x00, 0x00, 0x21, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x2c, 0x00,
            0x00, 0x00, 0x00, 0x1a, 0x8c, 0x00, 0x00, 0x02, 0x00,
        ];
        assert_eq!(parse_status_flags(&payload), Some(0x0200_008c));
        // RC жив (бит 7 стоит) vs тишина (0x0200_000c — бита нет)
        assert!(0x0200_008c & ARMING_FLAG_RC_ALIVE != 0);
        assert!(0x0200_000c & ARMING_FLAG_RC_ALIVE == 0);
    }
}

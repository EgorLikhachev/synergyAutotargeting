//! MSP v1 (Betaflight/INAV) — кодек фреймов для управления подвесом/полётом.
//! Порт wire-формата bkb `utils/uart_translator.py` (фаза D, ADR-012):
//!
//! `$M<` + `<len:u8>` + `<cmd:u8>` + `<payload LE>` + `<crc:u8>`,
//! crc = len ^ cmd ^ все байты payload (XOR).
//!
//! Основное сообщение — SET_RAW_RC (200): 16 RC-каналов, u16 LE, 1000..2000 мкс.

/// MSP_SET_RAW_RC (bkb: главный канал управления).
pub const MSP_SET_RAW_RC: u8 = 200;
/// MSP_RAW_GPS (запрос телеметрии GPS у полётника).
pub const MSP_RAW_GPS: u8 = 106;

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

/// MSP2_SET_ARMING (0x0323) — дословный порт `arm_packet` bkb:
/// `$M< 03 23 03 <01|00> crc`, crc = XOR всех байт фрейма (включая заголовок
/// и длину — особенность bkb, отличается от v1-правила).
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
    fn crc_matches_bkb_semantics() {
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
}

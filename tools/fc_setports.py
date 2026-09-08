#!/usr/bin/env python3
"""Настройка serial-портов FC через MSP, с верификацией ПОСЛЕ перезагрузки.

    python fc_setports.py COM4          # применить MSP на UART1-6 (до 4 попыток)
    python fc_setports.py COM4 --dry    # только показать текущее

Каждая попытка: чтение(54) → SET(55) с ack → EEPROM_WRITE(250) с ack →
REBOOT(68) → пауза → переподключение → контрольное чтение(54).
Успех = маска MSP на всех UART подтверждена ИМЕННО ПОСЛЕ перезагрузки.
Предыстория: readback до ребута читает RAM и доказывает только приём кадра;
пережить перезагрузку может только запись в EEPROM (слепая зона прошлой версии).

Формат (betaflight msp.c 4.4): MSP_CF_SERIAL_CONFIG=54 / SET=55:
  per-port: id u8, functionMask u16, msp/gps/telem/bb baudrateIndex u8;
  SET-кадр БЕЗ байта-счётчика — dataSize % 7 == 0, count = dataSize/7.
MSP=1 (FUNCTION_MSP), RX_SERIAL=64; VCP=20 не трогаем (оставляем MSP на USB).
"""
import struct
import sys
import time

import serial

FUNCTION_MSP = 1
FUNCTION_RX_SERIAL = 64
SKIP_PORTS = {20, 30, 31}  # VCP и softserial не трогаем
TARGET_IDS = {0, 1, 2, 3, 4, 5}  # UART1..UART6
ATTEMPTS = 4
REBOOT_WAIT_S = 6.0


def crc_v1(payload: bytes) -> int:
    c = 0
    for b in payload:
        c ^= b
    return c


def frame(cmd: int, data: bytes = b"") -> bytes:
    pl = bytes([len(data), cmd]) + data
    return b"$M<" + pl + bytes([crc_v1(pl)])


def read_frame(ser, timeout_s=1.5):
    ser.timeout = 0.1
    end = time.time() + timeout_s
    buf = b""
    while time.time() < end:
        b = ser.read(1)
        if not b:
            continue
        buf += b
        if buf.endswith(b"$M>"):
            while len(buf) < 5 and time.time() < end:
                d = ser.read(5 - len(buf))
                if d:
                    buf += d
            if len(buf) < 5:
                continue
            ln = buf[3]
            need = ln + 1
            while len(buf) < 5 + need and time.time() < end:
                d = ser.read(5 + need - len(buf))
                if d:
                    buf += d
            if len(buf) >= 5 + ln + 1:
                return buf[4], buf[5:5 + ln]
    return None


def parse_ports(data: bytes):
    ports = []
    for off in range(0, len(data) - 6, 7):
        pid, mask, msp, gps, tel, bb = struct.unpack_from("<BHB BBB".replace(" ", ""), data, off)
        ports.append(dict(id=pid, mask=mask, msp=msp, gps=gps, tel=tel, bb=bb))
    return ports


def open_port(port: str, tries: int = 20):
    for _ in range(tries):
        try:
            s = serial.Serial(port, 115200, timeout=0.1)
            s.reset_input_buffer()
            return s
        except serial.SerialException:
            time.sleep(0.5)
    return None


def read_config(ser):
    ser.write(frame(54))
    r = read_frame(ser)
    return parse_ports(r[1]) if r else None


def dump_ports(ports, prefix="  "):
    for p in ports:
        print(f"{prefix}id={p['id']:2d} mask=0x{p['mask']:04x} baud_idx: "
              f"msp={p['msp']} gps={p['gps']} tel={p['tel']} bb={p['bb']}")


def masks_ok(ports) -> bool:
    if not ports:
        return False
    return all((p["mask"] & FUNCTION_MSP) for p in ports if p["id"] in TARGET_IDS)


def attempt(port: str) -> bool:
    ser = open_port(port, tries=2)
    if not ser:
        print("  порт недоступен (Configurator держит?)")
        return False
    try:
        ports = read_config(ser)
        if ports is None:
            print("  нет ответа на чтение(54)")
            return False
        print("  текущие порты:")
        dump_ports(ports)
        if masks_ok(ports):
            print("  маски уже на месте (пережили загрузку) — готово")
            return True

        msp_idx = next((p["msp"] for p in ports if p["id"] == 20), 0)
        entries = b""
        for p in ports:
            if p["id"] in SKIP_PORTS:
                continue
            mask = FUNCTION_MSP | (p["mask"] & FUNCTION_RX_SERIAL)
            entries += struct.pack("<BH", p["id"], mask) + bytes(
                [msp_idx, p["gps"], p["tel"], p["bb"]]
            )

        ser.write(frame(55, entries))  # MSP_SET_CF_SERIAL_CONFIG
        ack = read_frame(ser, 1.5)
        print(f"  SET(55): {'ack получен' if ack else 'НЕТ ACK — запись отвергнута'}")
        if not ack:
            return False

        ser.write(frame(250))  # MSP_EEPROM_WRITE
        ack = read_frame(ser, 1.5)
        print(f"  EEPROM_WRITE(250): {'ack получен' if ack else 'НЕТ ACK'}")
        if not ack:
            return False

        ser.write(frame(68, b"\x00"))  # MSP_REBOOT
        ser.close()
        print(f"  REBOOT отправлен, жду {REBOOT_WAIT_S:.0f} c…")
        time.sleep(REBOOT_WAIT_S)

        ser2 = open_port(port)
        if not ser2:
            print("  не переподключился после ребута")
            return False
        try:
            ports2 = read_config(ser2)
            if masks_ok(ports2):
                print("  ПОСЛЕ РЕБУТА маски подтверждены:")
                dump_ports(ports2)
                return True
            print("  после ребута маски НЕ сохранились:")
            dump_ports(ports2 or [])
            return False
        finally:
            ser2.close()
    finally:
        try:
            ser.close()
        except Exception:
            pass


def main():
    port = sys.argv[1]
    dry = "--dry" in sys.argv
    if dry:
        ser = open_port(port, tries=2)
        if not ser:
            print("порт недоступен (Configurator держит?)")
            return 1
        try:
            ports = read_config(ser)
            if ports is None:
                print("нет ответа на чтение(54)")
                return 1
            dump_ports(ports)
        finally:
            ser.close()
        return 0

    for i in range(1, ATTEMPTS + 1):
        print(f"== попытка {i}/{ATTEMPTS}")
        try:
            if attempt(port):
                print("УСПЕХ: конфиг применён и подтверждён после перезагрузки")
                return 0
        except serial.SerialException as e:
            print(f"  ошибка порта: {e}")
        time.sleep(2)
    print("НЕ УДАЛОСЬ за все попытки — конфиг не переживает перезагрузку")
    return 1


if __name__ == "__main__":
    sys.exit(main())

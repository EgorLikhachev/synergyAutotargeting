#!/usr/bin/env python3
"""Опрос FC по MSP через виртуальный COM-порт (USB).

    python msp_query.py COM4 [status|rc]

Важно: Betaflight Configurator должен быть ОТКЛЮЧЕН (держит порт эксклюзивно).
"""
import struct
import sys
import time

import serial


def crc8_dvb_s2(data: bytes, crc: int = 0) -> int:  # не нужен для v1
    return crc


def crc_v1(payload: bytes) -> int:
    c = 0
    for b in payload:
        c ^= b
    return c


def frame(cmd: int, data: bytes = b"") -> bytes:
    pl = bytes([len(data), cmd]) + data
    return b"$M<" + pl + bytes([crc_v1(pl)])


def read_frame(ser, timeout_s=1.5) -> tuple[int, bytes] | None:
    ser.timeout = 0.1
    end = time.time() + timeout_s
    buf = b""
    while time.time() < end:
        b = ser.read(1)
        if not b:
            continue
        buf += b
        if buf.endswith(b"$M>"):
            # кадр: '$' 'M' '>' len cmd data... crc — дочитываем шапку
            while len(buf) < 5 and time.time() < end:
                d = ser.read(5 - len(buf))
                if d:
                    buf += d
            if len(buf) < 5:
                continue
            ln = buf[3]
            need = ln + 1  # data + crc
            while len(buf) < 5 + need and time.time() < end:
                d = ser.read(5 + need - len(buf))
                if d:
                    buf += d
            if len(buf) >= 5 + ln + 1:
                cmd = buf[4]
                data = buf[5:5 + ln]
                return cmd, data
    return None


def main():
    port = sys.argv[1]
    what = sys.argv[2] if len(sys.argv) > 2 else "status"
    ser = serial.Serial(port, 115200, timeout=0.1)
    ser.reset_input_buffer()

    if what == "status":
        ser.write(frame(101))  # MSP_STATUS
        r = read_frame(ser)
        if not r:
            print("НЕТ ОТВЕТА на MSP_STATUS")
            return 1
        _, d = r
        if len(d) >= 22:
            cycle, i2c, sens = struct.unpack_from("<HHH", d)
            modes, cpu = struct.unpack_from("<IH", d, 6)
            flags = struct.unpack_from("<I", d, len(d) - 4)[0] if len(d) >= 26 else None
            print(f"cycleTime={cycle} i2cErr={i2c} sensors=0x{sens:x} cpu={cpu}%")
            print(f"flightModes=0x{modes:x}")
            print(f"armingDisableFlags=0x{flags:08x}" if flags is not None else "флагов нет")
        else:
            print("status len", len(d), d.hex())
    elif what == "rc":
        ser.write(frame(105))  # MSP_RC
        r = read_frame(ser)
        if not r:
            print("НЕТ ОТВЕТА на MSP_RC")
            return 1
        _, d = r
        n = len(d) // 2
        ch = struct.unpack(f"<{n}H", d[:n * 2])
        print("каналы:", " ".join(f"{c:4d}" for c in ch))
    return 0


if __name__ == "__main__":
    sys.exit(main())

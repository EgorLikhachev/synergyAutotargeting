#!/usr/bin/env python3
"""CLI-сессия с FC через VCP (текст поверх COM-порта).

    python fc_cli.py COM4 "команда1" "команда2" ...
"""
import sys
import time

import serial


def main():
    port, cmds = sys.argv[1], sys.argv[2:]
    ser = serial.Serial(port, 115200, timeout=0.2)
    ser.reset_input_buffer()
    for cmd in cmds:
        ser.write((cmd + "\n").encode())
        ser.flush()
        out = b""
        t0 = time.time()
        while time.time() - t0 < 2.0:
            chunk = ser.read(256)
            if chunk:
                out += chunk
                t0 = time.time()  # продолжаем, пока идёт вывод
            if b"###ERROR" in out:
                break
        text = out.decode(errors="replace").replace("\r", "")
        print(f"--- $ {cmd}")
        for line in text.splitlines():
            if line.strip():
                print(line)
        if cmd.strip() == "save":
            time.sleep(4)  # перезагрузка FC после save
    return 0


if __name__ == "__main__":
    sys.exit(main())

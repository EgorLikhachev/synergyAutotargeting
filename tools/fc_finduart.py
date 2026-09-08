#!/usr/bin/env python3
"""Поиск UART полётника, к которому подключён ROCK 5A + валидный serial-конфиг.

Открытие (bf 4.4.3, GEPRCF405): config.c validateAndFixConfig() при КАЖДОЙ
загрузке сбрасывает ТОЛЬКО группу serialConfig, если isSerialConfigValid()
не проходит. Правила (io/serial.c + msp/msp_serial.h):
  - mspPortCount >= 1 и <= MAX_MSP_PORT_COUNT = 3 (VCP считается!);
  - VCP обязан иметь FUNCTION_MSP;
  - MSP на порту можно делить только с telemetry/blackbox/VTX_MSP.
Конфиг «MSP на всех портах» невалиден → сброс при каждой загрузке; EEPROM при
этом пишется корректно (фичи/имя сохраняются) — потому это и выглядело как
«настройка не сохраняется».

Алгоритм: пары UART ({0,1},{2,3},{4,5}) — на каждую ставим MSP на VCP+пару
(итого 3 MSP-порта, валидно), сохраняем, ребут, проверяем персистентность,
затем MSP_STATUS с борта (ssh ROCK 5A). Пара с ответом $M> = подключённый UART.

    python tools/fc_finduart.py COM4
"""
import os
import shutil
import struct
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import serial  # noqa: E402
from fc_setports import (  # noqa: E402
    FUNCTION_MSP, frame, open_port, parse_ports, read_frame,
)

PORT = sys.argv[1] if len(sys.argv) > 1 else 'COM4'
BOARD = 'radxa@192.168.0.224'
PAIRS = [(0, 1), (2, 3), (4, 5)]
SSH = shutil.which('ssh') or 'ssh'


def read_config(ser):
    ser.write(frame(54))
    r = read_frame(ser)
    return parse_ports(r[1]) if r else None


def apply_pair(pair):
    """MSP на VCP+пару → EEPROM → ребут. True = пережило перезагрузку."""
    ser = open_port(PORT, tries=2)
    if not ser:
        print('  порт недоступен (Configurator?)'); return False
    try:
        ports = read_config(ser)
        if ports is None:
            print('  нет ответа на 54'); return False
        msp_idx = next((p['msp'] for p in ports if p['id'] == 20), 0)
        entries = b''
        for p in ports:
            if p['id'] == 20:
                continue  # VCP не трогаем: там уже MSP
            mask = FUNCTION_MSP if p['id'] in pair else 0
            entries += struct.pack('<BH', p['id'], mask) + bytes(
                [msp_idx, p['gps'], p['tel'], p['bb']]
            )
        ser.write(frame(55, entries))
        if not read_frame(ser, 1.5):
            print('  SET(55): НЕТ ACK'); return False
        ser.write(frame(250))
        if not read_frame(ser, 1.5):
            print('  EEPROM(250): НЕТ ACK'); return False
        ser.write(frame(68, b'\x00'))
        ser.close()
        print('  SET+EEPROM+REBOOT ок, жду 6 с…')
        time.sleep(6)
        ser2 = open_port(PORT)
        if not ser2:
            print('  нет переподключения'); return False
        try:
            ports2 = read_config(ser2)
            if ports2 is None:
                print('  нет ответа после ребута'); return False
            want = {20} | set(pair)
            ok = all(
                bool(p['mask'] & FUNCTION_MSP) == (p['id'] in want)
                for p in ports2 if p['id'] != 30 and p['id'] != 31
            )
            print('  после ребута: ' + ', '.join(
                'id=%d:0x%04x' % (p['id'], p['mask']) for p in ports2))
            print('  персистентность:', 'ОК' if ok else 'СЛЕТЕЛО')
            return ok
        finally:
            ser2.close()
    finally:
        try:
            ser.close()
        except Exception:
            pass


def probe_board():
    """MSP_STATUS с борта через UART7. True = пришёл $M>."""
    cmd = [SSH, '-q', '-o', 'BatchMode=yes', BOARD,
           'bash ~/synergy/tools/msp_probe_board.sh']
    try:
        out = subprocess.run(cmd, capture_output=True, text=True,
                             timeout=90).stdout
    except Exception as e:
        print('  ssh ошибка:', e)
        return False
    tail = [l for l in out.splitlines() if l.strip()][-3:]
    for l in tail:
        print('  |', l)
    return 'ОК: получен' in out


def main():
    for pair in PAIRS:
        print('== пара UART%s (MSP на VCP+%s)' % (pair, list(pair)))
        try:
            if not apply_pair(pair):
                print('  пара пропущена (не применилась)')
                continue
        except serial.SerialException as e:
            print('  ошибка порта:', e)
            continue
        time.sleep(1)
        if probe_board():
            print('УСПЕХ: борт получил ответ $M> — подключён UART из пары %s' % (pair,))
            print('Финальный конфиг уже применён и персистентен: MSP на VCP и UART%s.' % (pair,))
            return 0
        print('  проба молчит — следующая пара')
    print('Все пары проверены, ответа нет: конфиг теперь персистентен,')
    print('но ни один UART не видит борт — проблема в проводке (см. инструкцию).')
    return 1


if __name__ == '__main__':
    sys.exit(main())

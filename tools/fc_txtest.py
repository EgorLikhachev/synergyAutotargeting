#!/usr/bin/env python3
"""Тест обратного пути FC → ROCK 5A через телеметрию LTM.

Идея: MSP-порт молчит, пока его не спросят — поэтому «проба запросом» не
отличает «провод не на RX» от «провод вообще не на UART». Телеметрия LTM
(FUNCTION_TELEMETRY_LTM = 1<<4 = 16, serial.h 4.4) передаёт кадры сама,
непрерывно. Включаем LTM на группах UART → слушаем /dev/ttyS7 на борту:
  - байты есть  → провод на TX какого-то UART из группы → изолируем порт
                  перебором, затем ставим MSP на найденный UART и пробуем
                  MSP_STATUS (ожидаем $M>);
  - байтов нет  → провод не на TX ни одного UART: пады/разъём не UART,
                  обрыв, либо земля не на G (проверить у человека).

    python tools/fc_txtest.py COM4
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
BOARD = 'radxa@192.168.0.225'
SSH = shutil.which('ssh') or 'ssh'
LTM = 1 << 4
BAUD_115200_IDX = 5


def read_config(ser):
    ser.write(frame(54))
    r = read_frame(ser)
    return parse_ports(r[1]) if r else None


def apply_config(mask_for):
    """mask_for(id) -> маска порта; VCP не трогаем. True = персистентно."""
    ser = open_port(PORT, tries=2)
    if not ser:
        print('  порт недоступен'); return False
    try:
        ports = read_config(ser)
        if ports is None:
            print('  нет ответа на 54'); return False
        msp_idx = next((p['msp'] for p in ports if p['id'] == 20), 0)
        entries = b''
        for p in ports:
            if p['id'] == 20:
                continue
            mask = mask_for(p['id'])
            tel = BAUD_115200_IDX if (mask & LTM) else p['tel']
            entries += struct.pack('<BH', p['id'], mask) + bytes(
                [msp_idx, p['gps'], tel, p['bb']])
        ser.write(frame(55, entries))
        if not read_frame(ser, 1.5):
            print('  SET(55): НЕТ ACK'); return False
        ser.write(frame(250))
        if not read_frame(ser, 1.5):
            print('  EEPROM(250): НЕТ ACK'); return False
        ser.write(frame(68, b'\x00'))
        ser.close()
        time.sleep(6)
        ser2 = open_port(PORT)
        if not ser2:
            print('  нет переподключения'); return False
        try:
            ports2 = read_config(ser2)
            if ports2 is None:
                print('  нет ответа после ребута'); return False
            ok = all(
                (p['mask'] == mask_for(p['id'])) if p['id'] != 20 else True
                for p in ports2 if p['id'] < 30
            )
            print('  после ребута: ' + ', '.join(
                'id=%d:0x%04x' % (p['id'], p['mask']) for p in ports2))
            if not ok:
                print('  КОНФИГ НЕ ПЕРСИСТЕНТЕН (валидация отвергла?)')
            return ok
        finally:
            ser2.close()
    finally:
        try:
            ser.close()
        except Exception:
            pass


def listen(secs=3):
    """Слушаем ttyS7 на борту. Возвращает число принятых байтов."""
    cmd = [SSH, '-q', '-o', 'BatchMode=yes', BOARD,
           'bash ~/synergy/tools/listen_board.sh %d' % secs]
    try:
        out = subprocess.run(cmd, capture_output=True, text=True,
                             timeout=120).stdout
    except Exception as e:
        print('  ssh ошибка:', e)
        return -1
    n = -1
    for line in out.splitlines():
        print('  |', line)
        if 'байтов принято:' in line:
            n = int(line.split(':')[1].strip())
    return n


def probe_msp():
    cmd = [SSH, '-q', '-o', 'BatchMode=yes', BOARD,
           'bash ~/synergy/tools/msp_probe_board.sh']
    try:
        out = subprocess.run(cmd, capture_output=True, text=True,
                             timeout=120).stdout
    except Exception as e:
        print('  ssh ошибка:', e)
        return False
    for line in out.splitlines():
        if line.strip():
            print('  |', line)
    return 'ОК: получен' in out


def main():
    print('== фаза 1: LTM на всех UART, слушаем борт 4 c')
    if not apply_config(lambda _id: LTM):
        print('  пробуем парами')
        groups = [(0, 1), (2, 3), (4, 5)]
    else:
        groups = [None]  # уже всё включено
    winner_group = None
    for g in groups:
        if g is not None:
            print('== фаза 1: LTM на паре %s' % (g,))
            if not apply_config(lambda _id, g=g: LTM if _id in g else 0):
                continue
        n = listen(4)
        if n > 0:
            winner_group = g if g is not None else (0, 1, 2, 3, 4, 5)
            break
        if n < 0:
            return 1
    if winner_group is None:
        print('ИТОГ: на TX ни одного UART тишина — провод не на UART / обрыв /')
        print('земля не на G. Нужны подписи падов на FC (см. сообщение).')
        return 1

    print('== фаза 2: изолируем UART внутри %s' % (winner_group,))
    found = None
    for uid in winner_group:
        print('  -- LTM только на UART id=%d' % uid)
        if not apply_config(lambda _id, uid=uid: LTM if _id == uid else 0):
            continue
        n = listen(3)
        if n > 0:
            found = uid
            break
    if found is None:
        print('ИТОГ: группа слышна, но одиночный порт не изолировался — странно,')
        print('покажи вывод разработчику.')
        return 1
    print('НАЙДЕН: провод на UART id=%d (TX). Ставим MSP на него.' % found)

    print('== фаза 3: финальный конфиг MSP на VCP+UART%d, проба MSP_STATUS' % found)
    if not apply_config(lambda _id, found=found:
                        FUNCTION_MSP if _id == found else 0):
        print('  финальный конфиг не применился')
        return 1
    if probe_msp():
        print('УСПЕХ: $M> с борта — двусторонняя связь FC<->ROCK 5A установлена.')
        return 0
    print('ИТОГ: TX путь есть (LTM слышали), но MSP-ответа нет — RX-провод')
    print('FC не на RX найденного UART. Проверить вторую сигнальную жилу.')
    return 1


if __name__ == '__main__':
    sys.exit(main())

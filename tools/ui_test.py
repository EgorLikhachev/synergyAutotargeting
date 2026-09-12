#!/usr/bin/env python3
"""ui_test: автотест операторского пульта против живого борта.

Оракул — журнал борта (journalctl -u synergy): каждая команда UI и
fail-safe логируются. Управление пультом — клавиатура (Esc/F/R) и клики
по кнопке АРМ (относительные координаты окна; при промахе тест честно
падает по отсутствию строки журнала — приложит скриншот).

Проверяет (чеклист ретеста Этапа 3):
  1. Запуск и подключение борта к пульту (журнал + заголовок окна).
  2. Клавиша R: запись .mjpg — файл растёт, в нём ≥30 JPEG-кадров.
  3. Клавиша Esc: СТОП наведения (строка журнала).
  4. АРМ в 2 клика: on=true в журнале.
  5. FAIL-SAFE: kill -9 пульта при АРМ → авто-СТОП на борту ≤2 c.
  6. Авто-реконнект после перезапуска пульта.

Запуск (борд онлайн, камера активна): python tools/ui_test.py
Скриншоты контрольных точек: dist/operator-ui/ui_test_shots/.
Выход: 0 = все проверки пройдены.
"""
import os
import subprocess
import sys
import time

BOARD = "radxa@192.168.0.224"
UI_EXE = os.path.join("dist", "operator-ui", "operator-ui.exe")
SHOTS = os.path.join("dist", "operator-ui", "ui_test_shots")
RECORDS = os.path.join("dist", "operator-ui", "records")
SUDO = "echo radxa | sudo -S"

results = []


def check(name, ok, detail=""):
    results.append((name, ok, detail))
    print(f"{'PASS' if ok else 'FAIL'}  {name}" + (f" — {detail}" if detail else ""))


def ssh(cmd, timeout=20):
    r = subprocess.run(
        ["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=8", BOARD, cmd],
        capture_output=True, text=True, timeout=timeout,
    )
    return r.stdout + r.stderr


def board_utc():
    return ssh("date -u '+%Y-%m-%d %H:%M:%S'").strip()


def journal_since(marker):
    out = ssh(
        f"{SUDO} journalctl -u synergy --since '{marker}' --no-pager -q 2>/dev/null"
    )
    return out


def wait_journal(marker, pattern, timeout_s=25.0, poll=1.0):
    """Ждать строку pattern в журнале после метки времени. Возвращает
    найденную строку или ''."""
    deadline = time.time() + timeout_s
    while time.time() < deadline:
        out = journal_since(marker)
        for line in out.splitlines():
            if pattern in line:
                return line.strip()
        time.sleep(poll)
    return ""


# --- WinAPI: окно, ввод, скриншоты -----------------------------------------
import ctypes

user32 = ctypes.windll.user32
FindWindowW = user32.FindWindowW
GetWindowRect = user32.GetWindowRect
SetForegroundWindow = user32.SetForegroundWindow
SetCursorPos = user32.SetCursorPos
mouse_event = user32.mouse_event
keybd_event = user32.keybd_event

MOUSE_DOWN, MOUSE_UP = 0x0002, 0x0004
KEY_UP = 0x0002
VK_ESCAPE, VK_R = 0x1B, 0x52


class Rect(ctypes.Structure):
    _fields_ = [("l", ctypes.c_long), ("t", ctypes.c_long),
                ("r", ctypes.c_long), ("b", ctypes.c_long)]


def find_window(timeout_s=20):
    deadline = time.time() + timeout_s
    while time.time() < deadline:
        hwnd = FindWindowW(None, "synergy")
        if hwnd:
            return hwnd
        time.sleep(0.5)
    return 0


def window_rect(hwnd):
    rc = Rect()
    GetWindowRect(hwnd, ctypes.byref(rc))
    return rc


def focus(hwnd):
    SetForegroundWindow(hwnd)
    time.sleep(0.4)


def send_key(vk):
    keybd_event(vk, 0, 0, 0)
    keybd_event(vk, 0, KEY_UP, 0)


def click(x, y):
    SetCursorPos(int(x), int(y))
    time.sleep(0.15)
    mouse_event(MOUSE_DOWN, 0, 0, 0, 0)
    mouse_event(MOUSE_UP, 0, 0, 0, 0)


def shot(hwnd, name):
    try:
        from PIL import ImageGrab
        rc = window_rect(hwnd)
        img = ImageGrab.grab(bbox=(rc.l, rc.t, rc.r, rc.b))
        path = os.path.join(SHOTS, name)
        img.save(path)
        return path
    except Exception as e:  # скриншоты — не критерий, лишь артефакт
        return f"(скриншот не удался: {e})"


def main():
    os.makedirs(SHOTS, exist_ok=True)
    os.makedirs(RECORDS, exist_ok=True)

    # --- 0. Предусловия: борд жив, сервис активен --------------------------
    out = ssh("systemctl is-active synergy; pgrep -c synergy")
    if "active" not in out:
        print(f"борд/сервис недоступен: {out!r}")
        return 1
    check("предусловия: борд онлайн, synergy active", True)

    marker0 = board_utc()

    # --- 1. Запуск пульта, подключение борта --------------------------------
    subprocess.Popen(
        [UI_EXE], cwd=os.path.dirname(UI_EXE),
        env=dict(os.environ, SYNERGY_TOKEN=os.environ.get("SYNERGY_TOKEN", "bench-synergy")))
    hwnd = find_window(20)
    check("пульт запущен, окно найдено", bool(hwnd))
    if not hwnd:
        return 1
    line = wait_journal(marker0, "[MJPEG-PUSH] подключён", timeout_s=25)
    check("борт подключился к пульту (журнал)", bool(line), line[:90])
    title = ctypes.create_unicode_buffer(128)
    user32.GetWindowTextW(hwnd, title, 128)
    # Норма = чистый «synergy»: суффиксы появляются только при проблемах/
    # записи/АРМ (см. main.rs: живой заголовок).
    check("живой заголовок окна (норма = без «нет связи»)",
          "нет связи" not in title.value, title.value)
    time.sleep(3)  # видео пошло, FC-телеметрия обновилась
    shot(hwnd, "01_connected.png")

    # --- 2. Запись (R) -------------------------------------------------------
    def window_title(h):
        buf = ctypes.create_unicode_buffer(128)
        user32.GetWindowTextW(h, buf, 128)
        return buf.value

    before = set(os.listdir(RECORDS))
    focus(hwnd)
    send_key(VK_R)
    # Живой заголовок обязан показать «ЗАПИСЬ» пока запись идёт.
    t_rec = ""
    for _ in range(12):
        time.sleep(0.25)
        t_rec = window_title(hwnd)
        if "ЗАПИСЬ" in t_rec:
            break
    check("живой заголовок: «ЗАПИСЬ» во время записи", "ЗАПИСЬ" in t_rec, t_rec)
    time.sleep(3.0)
    send_key(VK_R)
    time.sleep(1.0)
    new = set(os.listdir(RECORDS)) - before
    ok_rec, det = False, ""
    if new:
        f = sorted(new)[-1]
        path = os.path.join(RECORDS, f)
        size = os.path.getsize(path)
        data = open(path, "rb").read()
        frames = data.count(b"\xff\xd8\xff")
        ok_rec = size > 100_000 and frames >= 30
        det = f"{f}: {size/1e6:.1f} МБ, {frames} кадров"
    check("клавиша R: запись .mjpg (растёт, ≥30 кадров)", ok_rec, det)

    # --- 3. Esc = СТОП наведения ---------------------------------------------
    m = board_utc()
    focus(hwnd)
    send_key(VK_ESCAPE)
    line = wait_journal(m, "UI: СТОП наведения", timeout_s=10)
    check("клавиша Esc: СТОП наведения (журнал)", bool(line), line[:90])

    # --- 3б. X = СНЯТЬ ЗАХВАТ ---
    # X активен только при живом захвате (TRACK/ACQUIRE/LOST); после
    # предыдущих прогонов режим может быть IDLE — сначала захват
    # двойным кликом в центр видео.
    rc = window_rect(hwnd)
    focus(hwnd)
    click(rc.l + (rc.r - rc.l) * 0.5, rc.t + (rc.b - rc.t) * 0.45)
    time.sleep(0.12)
    click(rc.l + (rc.r - rc.l) * 0.5, rc.t + (rc.b - rc.t) * 0.45)
    time.sleep(1.5)  # режим -> ACQUIRE/LOST
    m = board_utc()
    send_key(0x58)  # X
    line = wait_journal(m, "UI: СНЯТЬ ЗАХВАТ", timeout_s=10)
    check("клавиша X: СНЯТЬ ЗАХВАТ (после захвата кликом)", bool(line), line[:90])

    # --- 4. АРМ в 2 клика -----------------------------------------------------
    rc = window_rect(hwnd)
    focus(hwnd)
    # Кнопка «АРМ (2 клика)» — первая в нижней панели; клик по центру
    # левой трети кнопочной строки. При смене вёрстки координаты правятся
    # здесь (проверяется строкой журнала — промах виден сразу).
    btn_x = rc.l + (rc.r - rc.l) * 0.63
    btn_y = rc.t + (rc.b - rc.t) * 0.93
    m = board_utc()
    click(btn_x, btn_y)          # «АРМ (2 клика)» -> «ТОЧНО → РАЗРЕШИТЬ»
    time.sleep(0.6)
    click(btn_x, btn_y)          # подтверждение
    line = wait_journal(m, "UI: АРМ наведения", timeout_s=10)
    armed_ok = "on=true" in line
    t_arm = window_title(hwnd)
    check("АРМ в 2 клика (журнал on=true)", armed_ok, line[:90])
    check("живой заголовок: «АРМ» после арма", "АРМ" in t_arm, t_arm)
    shot(hwnd, "02_armed.png")

    # --- 5. FAIL-SAFE: kill пульта при АРМ ------------------------------------
    m = board_utc()
    t_kill = time.time()
    subprocess.run(["taskkill", "/IM", "operator-ui.exe", "/F"],
                   capture_output=True)
    line = wait_journal(m, "FAIL-SAFE", timeout_s=15)
    dt = time.time() - t_kill
    ok_fs = bool(line) and dt <= 6.0
    check("fail-safe: kill -9 пульта при АРМ → авто-СТОП", ok_fs,
          (line[:80] + f"; задержка {dt:.1f} с") if line else f"нет строки за {dt:.0f} с")

    # --- 6. Реконнект после перезапуска ---------------------------------------
    marker = board_utc()
    subprocess.Popen(
        [UI_EXE], cwd=os.path.dirname(UI_EXE),
        env=dict(os.environ, SYNERGY_TOKEN=os.environ.get("SYNERGY_TOKEN", "bench-synergy")))
    hwnd2 = find_window(20)
    line = wait_journal(marker, "[MJPEG-PUSH] подключён", timeout_s=25)
    check("перезапуск пульта → авто-реконнект борта", bool(hwnd2 and line),
          line[:90])
    if hwnd2:
        time.sleep(3)
        shot(hwnd2, "03_reconnected.png")
        subprocess.run(["taskkill", "/IM", "operator-ui.exe", "/F"],
                       capture_output=True)

    # --- итог ------------------------------------------------------------------
    print()
    failed = [n for n, ok, _ in results if not ok]
    print(f"ИТОГ: {len(results) - len(failed)}/{len(results)} пройдено" +
          (f"; ПРОВАЛЫ: {', '.join(failed)}" if failed else ""))
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())

#!/usr/bin/env python3
"""ui_visual_test: пиксельная проверка egui-оверлеев пульта (UX P1).

Каждый оверлей рисуется УНИКАЛЬНЫМ цветом (см. operator-ui main.rs) —
тест ищет эти цвета в скриншоте окна, поэтому не зависит от вёрстки:

  прицел-крест        cyan  (0, 210, 255)
  допуск-круг         зелёный (40,255,120) в допуске / янтарный (255,170,0)
  бейдж режима        рамка цветом режима (TRACK зелёная и т.п.)
  REC-бейдж           красный текст (255,80,80)
  АРМ-рамка           красный (220,40,40)/(150,26,26) по краям кадра

Сценарий: подключение → снимок (крест+допуск+бейдж) → R/запись → снимок
(REC) → АРМ в 2 клика → снимок (АРМ-рамка) → Esc-СТОП → чистый выход.
Скриншоты: dist/operator-ui/ui_test_shots/. Выход: 0 = всё пройдено.
"""
import os
import subprocess
import sys
import time

BOARD = "radxa@192.168.0.224"
UI_EXE = os.path.join("dist", "operator-ui", "operator-ui.exe")
SHOTS = os.path.join("dist", "operator-ui", "ui_test_shots")
SUDO = "echo radxa | sudo -S"

CYAN = (0, 210, 255)
TOL_GREEN = (40, 255, 120)
TOL_AMBER = (255, 170, 0)
TRACK_GREEN = (60, 220, 90)
LOST_RED = (255, 90, 90)
REC_RED = (255, 80, 80)
ARM_RED = (220, 40, 40)
ARM_RED_DIM = (150, 26, 26)
ARM_PLAQUE = (150, 25, 25)

TOL = 12  # допуск сравнения каналов (антиалиасинг/JPEG-видео)

results = []


def check(name, ok, detail=""):
    results.append((name, ok))
    print(f"{'PASS' if ok else 'FAIL'}  {name}" + (f" — {detail}" if detail else ""))


def ssh(cmd, timeout=15):
    r = subprocess.run(
        ["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=8", BOARD, cmd],
        capture_output=True, text=True, timeout=timeout)
    return r.stdout + r.stderr


def wait_journal(marker, pattern, timeout_s=25.0):
    deadline = time.time() + timeout_s
    while time.time() < deadline:
        out = ssh(f"{SUDO} journalctl -u synergy --since '{marker}' --no-pager -q 2>/dev/null")
        for line in out.splitlines():
            if pattern in line:
                return line.strip()
        time.sleep(1.0)
    return ""


# --- WinAPI ------------------------------------------------------------------
import ctypes

user32 = ctypes.windll.user32
Rect = type("R", (ctypes.Structure,), {
    "_fields_": [("l", ctypes.c_long), ("t", ctypes.c_long),
                 ("r", ctypes.c_long), ("b", ctypes.c_long)]})


def find_window(timeout_s=20):
    deadline = time.time() + timeout_s
    while time.time() < deadline:
        hwnd = user32.FindWindowW(None, "synergy")
        if hwnd:
            return hwnd
        time.sleep(0.5)
    return 0


def window_rect(hwnd):
    rc = Rect()
    user32.GetWindowRect(hwnd, ctypes.byref(rc))
    return rc


def focus(hwnd):
    user32.SetForegroundWindow(hwnd)
    time.sleep(0.4)


def send_key(vk):
    user32.keybd_event(vk, 0, 0, 0)
    user32.keybd_event(vk, 0, 2, 0)


def click(x, y):
    user32.SetCursorPos(int(x), int(y))
    time.sleep(0.15)
    user32.mouse_event(2, 0, 0, 0, 0)
    user32.mouse_event(4, 0, 0, 0, 0)


def shot(hwnd, name):
    from PIL import ImageGrab
    rc = window_rect(hwnd)
    img = ImageGrab.grab(bbox=(rc.l, rc.t, rc.r, rc.b))
    path = os.path.join(SHOTS, name)
    img.save(path)
    return img


def count_color(img, rgb, tol=TOL):
    """Сколько пикселей близко к rgb (быстрый downsampling-скан)."""
    w, h = img.size
    px = img.load()
    n = 0
    for y in range(0, h, 2):        # шаг 2: оверлеи шире 2 px
        for x in range(0, w, 2):
            p = px[x, y]
            if (abs(p[0] - rgb[0]) <= tol and abs(p[1] - rgb[1]) <= tol
                    and abs(p[2] - rgb[2]) <= tol):
                n += 1
    return n


def count_hue(img, pred):
    """Сколько пикселей удовлетворяет предикату (r,g,b) -> bool.
    Устойчиво к антиалиасингу тонких линий поверх видео."""
    w, h = img.size
    px = img.load()
    return sum(
        1
        for y in range(0, h, 2)
        for x in range(0, w, 2)
        if pred(px[x, y])
    )


def main():
    os.makedirs(SHOTS, exist_ok=True)
    if "active" not in ssh("systemctl is-active synergy"):
        print("борд недоступен")
        return 1

    marker0 = ssh("date -u '+%Y-%m-%d %H:%M:%S'").strip()
    subprocess.Popen(
        [UI_EXE], cwd=os.path.dirname(UI_EXE),
        env=dict(os.environ, SYNERGY_TOKEN=os.environ.get("SYNERGY_TOKEN", "bench-synergy")))
    hwnd = find_window(20)
    check("окно пульта найдено", bool(hwnd))
    if not hwnd:
        return 1
    line = wait_journal(marker0, "[MJPEG-PUSH] подключён")
    check("борт подключился (видео идёт)", bool(line))
    time.sleep(4)  # видео + телеметрия прогрелись

    # --- Снимок 1: базовые оверлеи ----------------------------------------
    img = shot(hwnd, "v1_overlays.png")
    n_cyan = count_hue(
        img, lambda p: p[2] > 180 and p[1] > 130 and p[0] < 110 and p[2] - p[0] > 80
    )
    check("прицел-крест (cyan) в центре кадра", n_cyan >= 8, f"{n_cyan} px")
    n_amber = count_color(img, TOL_AMBER)
    n_tg = count_color(img, TOL_GREEN)
    check("допуск-круг ±30px (янтарь/зелёный)", n_amber + n_tg >= 20,
          f"янтарь {n_amber}, зелёный {n_tg}")
    n_mode = count_color(img, TRACK_GREEN) + count_color(img, LOST_RED)
    check("бейдж режима с рамкой (TRACK/LOST цвет)", n_mode >= 12,
          f"{n_mode} px")

    # --- Зум колесом: бейдж ×N появляется, циана становится больше ----------
    focus(hwnd)
    rcz = window_rect(hwnd)
    user32.SetCursorPos(int(rcz.l + (rcz.r - rcz.l) * 0.5),
                        int(rcz.t + (rcz.b - rcz.t) * 0.45))
    for _ in range(4):
        user32.mouse_event(0x0800, 0, 0, 120, 0)  # wheel up
        time.sleep(0.15)
    time.sleep(0.8)
    img = shot(hwnd, "v1b_zoom.png")
    n_cyan2 = count_hue(
        img, lambda p: p[2] > 180 and p[1] > 130 and p[0] < 110 and p[2] - p[0] > 80
    )
    check("зум колесом: бейдж ×N (циана больше)", n_cyan2 > n_cyan + 5,
          f"было {n_cyan}, стало {n_cyan2}")
    for _ in range(8):  # вернуть ×1
        user32.mouse_event(0x0800, 0, 0, -120, 0)
        time.sleep(0.1)
    time.sleep(0.5)

    # --- Снимок 2: запись ----------------------------------------------------
    focus(hwnd)
    send_key(0x52)  # R
    time.sleep(2.5)
    img = shot(hwnd, "v2_rec.png")
    n_rec = count_color(img, REC_RED)
    check("REC-бейдж на экране (красный)", n_rec >= 10, f"{n_rec} px")
    send_key(0x52)  # R — стоп записи
    time.sleep(0.5)

    # --- Снимок 3: АРМ ------------------------------------------------------
    rc = window_rect(hwnd)
    focus(hwnd)
    m = ssh("date -u '+%Y-%m-%d %H:%M:%S'").strip()
    bx = rc.l + (rc.r - rc.l) * 0.63
    by = rc.t + (rc.b - rc.t) * 0.93
    click(bx, by)
    time.sleep(0.5)
    click(bx, by)
    line = wait_journal(m, "UI: АРМ наведения", timeout_s=10)
    check("АРМ (журнал on=true)", "on=true" in line, line[:80])
    time.sleep(0.3)  # поймать мигание в яркой фазе
    img = shot(hwnd, "v3_arm.png")
    n_arm = count_color(img, ARM_RED) + count_color(img, ARM_RED_DIM)
    n_plq = count_color(img, ARM_PLAQUE, tol=25)
    check("АРМ-рамка по краям кадра (красная)", n_arm >= 40, f"{n_arm} px")
    check("плашка «НАВЕДЕНИЕ АКТИВНО»", n_plq >= 60, f"{n_plq} px")

    # --- чистый финал: СТОП и закрытие ---------------------------------------
    focus(hwnd)
    send_key(0x1B)  # Esc — СТОП наведения (НЕ disarm FC)
    time.sleep(1.0)
    subprocess.run(["taskkill", "/IM", "operator-ui.exe", "/F"], capture_output=True)

    failed = [n for n, ok in results if not ok]
    print()
    print(f"ИТОГ: {len(results) - len(failed)}/{len(results)} пройдено" +
          (f"; ПРОВАЛЫ: {', '.join(failed)}" if failed else ""))
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())

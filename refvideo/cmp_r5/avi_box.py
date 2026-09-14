#!/usr/bin/env python3
"""R5: динамика рамки трекера по записи пульта (AVI M-JPEG).

Рамка вшита в видео бортовым OSD (TRACK_GREEN 60,220,90). Кольцо допуска
пульта (TOL_GREEN 40,255,120) отделяем по цвету (G<250, B<115). Для каждой
строки берём самый длинный горизонтальный зелёный прогон: строки с прогоном
>= 15 px — края бокса (у дуг кольца прогоны короткие).

  python avi_box.py <файл.avi> [шаг_кадров]
"""
import io
import struct
import sys

import numpy as np
from PIL import Image

TRACK = np.array([60, 220, 90], dtype=np.int16)
TOL = np.array([40, 255, 120], dtype=np.int16)


def avi_frames(path):
    data = open(path, "rb").read()
    n = len(data)
    frames = []

    def u32(p):
        return struct.unpack_from("<I", data, p)[0]

    def walk(pos, end):
        while pos + 8 <= end:
            tag = bytes(data[pos:pos + 4])
            size = u32(pos + 4)
            body = pos + 8
            stop = min(body + size, end)
            if tag in (b"RIFF", b"LIST") and stop - body >= 4:
                walk(body + 4, stop)  # пропускаем fourcc ('movi' и т.п.)
            elif tag in (b"00dc", b"00db") and size > 0:
                frames.append((len(frames), data[body:stop]))
            pos = stop + (size & 1)

    walk(0, n)
    return frames


def box_of(img):
    a = np.asarray(img, dtype=np.int16)
    d_track = np.abs(a - TRACK).sum(axis=2)
    d_tol = np.abs(a - TOL).sum(axis=2)
    mask = (d_track < 120) & (d_track < d_tol)
    if mask.sum() < 40:
        return None
    h, w = mask.shape
    best_run, run_rows = 0, []
    for y in range(h):
        row = mask[y]
        # самый длинный подряд идущий True
        idx = np.flatnonzero(np.diff(np.concatenate(([0], row.view(np.int8), [0]))))
        if len(idx):
            runs = idx[1::2] - idx[0::2]
            r = int(runs.max())
            if r >= 15:
                run_rows.append(y)
                best_run = max(best_run, r)
    if not run_rows:
        return None
    top, bot = run_rows[0], run_rows[-1]
    return {"w": best_run, "h": bot - top + 1, "top": top,
            "rows": len(run_rows), "px": int(mask.sum())}


def main():
    path = sys.argv[1]
    step = int(sys.argv[2]) if len(sys.argv) > 1 and len(sys.argv) > 2 else 5
    frames = avi_frames(path)
    print(f"кадров в AVI: {len(frames)}, шаг выборки: {step} "
          f"(~{30.0 / step:.1f} замеров/с)")
    rec = []
    for idx, jp in frames[::step]:
        try:
            img = Image.open(io.BytesIO(jp)).convert("RGB")
        except Exception:
            continue
        b = box_of(img)
        rec.append((idx, b))
        if b:
            print(f"кадр {idx:5d}  t={idx / 30.0:6.1f}s  w={b['w']:4d} h={b['h']:4d} "
                  f"top={b['top']:3d}")
        else:
            print(f"кадр {idx:5d}  t={idx / 30.0:6.1f}s  —")
    ws = [(i, b["w"]) for i, b in rec if b]
    if ws:
        import statistics as st
        vals = [w for _, w in ws]
        print(f"\nзамеров с боксом: {len(ws)}/{len(rec)}; w: мед={st.median(vals):.0f} "
              f"мин={min(vals)} макс={max(vals)}")
    # сохранить пару кадров для просмотра
    for idx, jp in frames[::max(1, len(frames) // 6)][:6]:
        try:
            Image.open(io.BytesIO(jp)).convert("RGB").save(f"frame_{idx:06d}.png")
        except Exception:
            pass


if __name__ == "__main__":
    main()

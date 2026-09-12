#!/usr/bin/env python3
"""Извлечение приближённого ground-truth трека цели из эталонных видео (v2).

Записи сделаны системой, ВЕДУЩЕЙ цель ⇒ цель близка к центру кадра, фон
движется. На каждом кадре: top-hat (пятно против фона) в окне ±120 px от
центра, лучший блоб с контрастом 15..200 (не артефакты компрессии 255) и
размером 3..40 px. Без накопления дрейфа — якорь всегда центр.

Выход: <имя>.gt.jsonl — {"frame": n, "x", "y", "w", "h", "contrast"} или
{"frame": n, "x": null} если кандидата нет.
"""
import cv2
import glob
import json
import os
import sys

import numpy as np

SRC = r"dataset/raw"  # каталог роликов для разметки
R = 120  # окно поиска от центра


def best_blob(g_inv: np.ndarray):
    """Лучшее маленькое пятно в центральном окне инвертированного кадра."""
    h, w = g_inv.shape
    cx, cy = w // 2, h // 2
    x0, x1 = max(0, cx - R), min(w, cx + R)
    y0, y1 = max(0, cy - R), min(h, cy + R)
    win = g_inv[y0:y1, x0:x1].astype(np.float32)
    blur = cv2.GaussianBlur(win, (41, 41), 0)
    score = cv2.morphologyEx(
        np.abs(win - blur).astype(np.uint8), cv2.MORPH_TOPHAT, np.ones((5, 5), np.uint8)
    ).astype(np.float32)
    _, mx, _, loc = cv2.minMaxLoc(score)
    if not (15.0 <= mx <= 200.0):
        return None
    bx, by = loc
    m = score > max(mx * 0.5, 8)
    _, lbl = cv2.connectedComponents(m.astype(np.uint8))
    comp = int(lbl[by, bx])
    if comp == 0:
        return None
    ys, xs = np.where(lbl == comp)
    bw = xs.max() - xs.min() + 1
    bh = ys.max() - ys.min() + 1
    if not (3 <= bw <= 40 and 3 <= bh <= 40):
        return None
    return {
        "x": round(float(x0 + xs.mean()), 1),
        "y": round(float(y0 + ys.mean()), 1),
        "w": int(bw), "h": int(bh), "contrast": round(float(mx), 1),
    }


def main():
    outdir = os.path.join(os.path.dirname(os.path.abspath(__file__)), "gt")
    os.makedirs(outdir, exist_ok=True)
    files = sorted(glob.glob(os.path.join(SRC, "*", "*.mp4")))
    for f in files:
        tag = ("day" if "Дневная" in f else "th") + "_" + f[-10:-4]
        cap = cv2.VideoCapture(f)
        rows = []
        i = 0
        while True:
            ok, fr = cap.read()
            if not ok:
                break
            g = cv2.cvtColor(fr, cv2.COLOR_BGR2GRAY)
            if "day" in tag:
                g = 255 - g  # тёмный силуэт → яркое пятно
            b = best_blob(g)
            row = {"frame": i}
            if b:
                row.update(b)
            else:
                row["x"] = None
            rows.append(row)
            i += 1
        cap.release()
        found = sum(1 for r in rows if r["x"] is not None)
        with open(os.path.join(outdir, tag + ".gt.jsonl"), "w") as w:
            for r in rows:
                w.write(json.dumps(r) + "\n")
        print(tag, f"{found}/{len(rows)} кадров с целью", flush=True)


if __name__ == "__main__":
    sys.exit(main())

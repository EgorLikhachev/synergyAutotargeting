#!/usr/bin/env python3
"""Сводка телеметрии прогона synergy — одним запуском (подготовка фазы A).

    python tools/telemetry_report.py [путь/telemetry.jsonl]

Считает: распределение режимов, score/track_ms/fps (медиана, p95),
стабильность бокса (IoU соседних TRACK-кадров), циклы потери/повторного
захвата с латентностью, дет-кадры (mode=ACQUIRE или det_ms присутствует).
"""
import json
import math
import statistics
import sys
from collections import Counter


def load(path):
    rows = []
    with open(path, encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if line:
                try:
                    rows.append(json.loads(line))
                except json.JSONDecodeError:
                    pass
    return rows


def iou(a, b):
    ax2, ay2 = a["x"] + a["w"], a["y"] + a["h"]
    bx2, by2 = b["x"] + b["w"], b["y"] + b["h"]
    ix = max(0, min(ax2, bx2) - max(a["x"], b["x"]))
    iy = max(0, min(ay2, by2) - max(a["y"], b["y"]))
    inter = ix * iy
    if inter == 0:
        return 0.0
    u = a["w"] * a["h"] + b["w"] * b["h"] - inter
    return inter / u


def pct(v, q):
    v = sorted(v)
    return v[min(len(v) - 1, int(len(v) * q))]


def main():
    path = sys.argv[1] if len(sys.argv) > 1 else "data/telemetry.jsonl"
    rows = load(path)
    if not rows:
        sys.exit(f"пустая телеметрия: {path}")
    modes = Counter(r["mode"] for r in rows)
    total = len(rows)
    track = [r for r in rows if r["mode"] == "TRACK" and r.get("x") is not None]

    print(f"== {path}: {total} кадров ==")
    print("режимы:", ", ".join(f"{m} {c} ({100 * c / total:.1f}%)" for m, c in modes.most_common()))

    if track:
        sc = [r["score"] for r in track]
        tm = [r["track_ms"] for r in track]
        fps = [r["fps"] for r in rows if r.get("fps")]
        print(f"score:      min {min(sc):.3f} | медиана {statistics.median(sc):.3f} | max {max(sc):.3f}")
        print(f"track_ms:   медиана {statistics.median(tm):.2f} | p95 {pct(tm, 0.95):.2f}")
        if fps:
            print(f"fps:        медиана {statistics.median(fps):.1f} | min {min(fps):.1f}")

        # Стабильность бокса: IoU с предыдущим TRACK-кадром (первые ~секунды пропускаем).
        ious = [
            iou(a, b)
            for a, b in zip(track, track[1:])
            if b["frame_seq"] - a["frame_seq"] <= 5
        ]
        if ious:
            print(f"IoU соседних: медиана {statistics.median(ious):.3f} | p05 {pct(ious, 0.05):.3f}")

    # Циклы потери → повторный захват: TRACK* → (ACQUIRE|LOST)+ → TRACK
    reacq, lost_started = [], None
    prev_mode = None
    for r in rows:
        m = r["mode"]
        if prev_mode == "TRACK" and m in ("ACQUIRE", "LOST"):
            lost_started = r["ts_ms"]
        if lost_started is not None and m == "TRACK":
            reacq.append(r["ts_ms"] - lost_started)
            lost_started = None
        prev_mode = m
    if reacq:
        print(f"повторных захватов: {len(reacq)}, латентность мс: "
              f"медиана {statistics.median(reacq):.0f} | max {max(reacq):.0f}")
    else:
        print("повторных захватов: 0 (потерь не было)")

    det = [r for r in rows if r.get("det_ms") is not None]
    if det:
        dm = [r["det_ms"] for r in det]
        print(f"дет-инференсов: {len(det)}, det_ms: медиана {statistics.median(dm):.1f}")

    # Скачки телепортации бокса (>100 px/кадр) — маркер срыва трекера.
    if track:
        jumps = sum(
            1
            for a, b in zip(track, track[1:])
            if b["frame_seq"] - a["frame_seq"] <= 5
            and math.hypot(
                (b["x"] + b["w"] / 2) - (a["x"] + a["w"] / 2),
                (b["y"] + b["h"] / 2) - (a["y"] + a["h"] / 2),
            )
            > 100
        )
        print(f"телепортов бокса (>100px/кадр): {jumps}")


if __name__ == "__main__":
    main()

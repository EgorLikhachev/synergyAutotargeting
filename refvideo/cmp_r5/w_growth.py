#!/usr/bin/env python3
"""R5: рост рамки трекера в replay-прогонах (до/после ADR-027).

Для каждого прогона по telemetry.jsonl (кадры TRACK с боксом):
  w_med      — медиана ширины бокса за ролик;
  w_p95      — 95-й перцентиль;
  w_start    — медиана первых 20 боксов (норма после ре-якора);
  w_max      — максимум;
  grow_x     — w_max / w_start: во сколько раз рамка раздавалась;
  frac>80px  — доля кадров с w > 80 px (цели 7-30 px, 640-кадр);
  lost_cnt   — переходы TRACK->LOST (edge-проверка/скор).

Запуск: python w_growth.py <root1> [<root2> ...]   # root = <name>/runs/<ts>/
"""
import json
import os
import statistics as st
import sys


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


def analyze(run_dir):
    tel = load(os.path.join(run_dir, "telemetry.jsonl"))
    ws, lost, prev_track = [], 0, False
    for t in tel:
        if t["mode"] == "TRACK" and t.get("w") is not None:
            ws.append(float(t["w"]))
            prev_track = True
        else:
            if prev_track and t["mode"] == "LOST":
                lost += 1
            prev_track = t["mode"] == "TRACK"
    if not ws:
        return None
    ws.sort()
    start = st.median(ws[:20]) if len(ws) >= 20 else st.median(ws)
    p95 = ws[max(0, int(0.95 * len(ws)) - 1)]
    return {
        "n": len(ws),
        "w_med": st.median(ws),
        "w_p95": p95,
        "w_start": start,
        "w_max": ws[-1],
        "grow": ws[-1] / start if start > 0 else float("inf"),
        "frac80": sum(1 for w in ws if w > 80) / len(ws),
        "lost": lost,
    }


def main():
    print(f"{'ролик':14s} {'кадры':>6s} {'w_нач':>6s} {'w_мед':>6s} {'w_p95':>6s} "
          f"{'w_max':>6s} {'рост':>5s} {'>80px':>6s} {'LOST':>5s}")
    for root in sys.argv[1:]:
        label = os.path.basename(root.rstrip("/\\")) or root
        for name in sorted(os.listdir(root)):
            sub = os.path.join(root, name, "runs")
            if not os.path.isdir(sub):
                continue
            runs = sorted(os.listdir(sub))
            if not runs:
                continue
            r = analyze(os.path.join(sub, runs[-1]))
            if r is None:
                print(f"{name:14s} нет TRACK-кадров")
                continue
            print(f"{name:14s} {r['n']:6d} {r['w_start']:6.1f} {r['w_med']:6.1f} "
                  f"{r['w_p95']:6.1f} {r['w_max']:6.1f} {r['grow']:5.1f}x "
                  f"{100 * r['frac80']:5.0f}% {r['lost']:5d}")
        print()


if __name__ == "__main__":
    main()

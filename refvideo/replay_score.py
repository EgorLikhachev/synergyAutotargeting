#!/usr/bin/env python3
"""Скоринг replay-прогонов по эталонным видео против ground-truth.

Сопоставляет для каждого прогона:
  - raw_detections.jsonl (детектор, до порога) с GT (полнота детекции,
    разделение conf цель/фон → рекомендация порога);
  - telemetry.jsonl (трекер) с GT (покрытие TRACK, ошибка центра бокса).

Запуск (после прогона run_replays.sh и скачивания каталога):
  python replay_score.py <board_run_root> <gt_dir>
Пример:
  python replay_score.py ../scp/replay_runs ../gt
"""
import json
import os
import sys

import statistics as st

TOL = 35.0  # px: считаем попадание, если центр бокса в TOL от центра GT


def load_jsonl(path):
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


def center(x, y, w, h):
    return (x + w / 2.0, y + h / 2.0)


def score_one(run_dir, gt_path):
    gt = {}
    for r in load_jsonl(gt_path):
        if r.get("x") is not None:
            gt[r["frame"]] = center(r["x"], r["y"], r["w"], r["h"])

    raw = load_jsonl(os.path.join(run_dir, "raw_detections.jsonl"))
    tel = load_jsonl(os.path.join(run_dir, "telemetry.jsonl"))

    # --- детекция: лучший conf на кадр; совпадение с GT по TOL ---
    best = {}
    for d in raw:
        c = center(d["x"], d["y"], d["w"], d["h"])
        prev = best.get(d["frame_seq"])
        if prev is None or d["conf"] > prev[0]:
            best[d["frame_seq"]] = (d["conf"], c)

    hit_conf, miss_conf = [], []
    n_det_any = 0
    for frame, (gtx, gty) in gt.items():
        conf, c = best.get(frame, (None, None))
        if conf is None:
            continue
        n_det_any += 1
        if abs(c[0] - gtx) <= TOL and abs(c[1] - gty) <= TOL:
            hit_conf.append(conf)
        else:
            miss_conf.append(conf)

    # --- трекинг: кадры TRACK с центром рядом с GT ---
    n_track_ok = n_track = 0
    errs = []
    for t in tel:
        if t["mode"] != "TRACK" or t.get("x") is None:
            continue
        n_track += 1
        g = gt.get(t["frame_seq"])
        if g is None:
            continue
        cx, cy = center(t["x"], t["y"], t["w"], t["h"])
        if abs(cx - g[0]) <= TOL and abs(cy - g[1]) <= TOL:
            n_track_ok += 1
            errs.append(((cx - g[0]) ** 2 + (cy - g[1]) ** 2) ** 0.5)

    res = {
        "gt_frames": len(gt),
        "det_frames_any": n_det_any,
        "det_on_target": len(hit_conf),
        "conf_on_target_med": st.median(hit_conf) if hit_conf else None,
        "conf_off_target_med": st.median(miss_conf) if miss_conf else None,
        "track_frames": n_track,
        "track_on_target": n_track_ok,
        "track_err_med_px": round(st.median(errs), 1) if errs else None,
    }
    # рекомендация порога: разделение hit/miss по квартилям
    if hit_conf and miss_conf:
        lo = sorted(hit_conf)[len(hit_conf) // 4]
        hi = sorted(miss_conf)[3 * len(miss_conf) // 4]
        res["conf_hint"] = f"[{lo:.2f}..{hi:.2f}] (Q1 попаданий .. Q3 фоновых)"
    return res


def main():
    run_root, gt_dir = sys.argv[1], sys.argv[2]
    rows = []
    for name in sorted(os.listdir(run_root)):
        # каталог прогона: <name>/runs/<timestamp>/
        sub = os.path.join(run_root, name, "runs")
        if not os.path.isdir(sub):
            continue
        runs = sorted(os.listdir(sub))
        if not runs:
            continue
        run_dir = os.path.join(sub, runs[-1])
        gt_path = os.path.join(gt_dir, name + ".gt.jsonl")
        if not os.path.exists(gt_path):
            continue
        r = score_one(run_dir, gt_path)
        rows.append((name, r))

    print(f"{'ролик':14s} {'GT':>5s} {'дет-any':>7s} {'дет-цель':>8s} "
          f"{'conf+/conf-':>13s} {'трек-цель':>9s} {'ошибка':>6s}")
    for name, r in rows:
        c1 = f"{r['conf_on_target_med']:.2f}" if r["conf_on_target_med"] else "-"
        c2 = f"{r['conf_off_target_med']:.2f}" if r["conf_off_target_med"] else "-"
        te = str(r["track_err_med_px"]) if r["track_err_med_px"] is not None else "-"
        print(f"{name:14s} {r['gt_frames']:5d} {r['det_frames_any']:7d} "
              f"{r['det_on_target']:8d} {c1 + '/' + c2:>13s} "
              f"{r['track_on_target']:9d} {te:>6s}")
        if "conf_hint" in r:
            print(f"{'':14s} порог: {r['conf_hint']}")


if __name__ == "__main__":
    main()

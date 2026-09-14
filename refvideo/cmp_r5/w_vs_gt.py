#!/usr/bin/env python3
"""R5: отношение w трекера к w GT — честный тест раздутия рамки.

Рост w легитимен, только если растёт и цель (приближение). Считаем по
TRACK-кадрам с GT: ratio = w_track / w_gt; медиана/p95, доля кадров с
раздуванием (ratio > 2), и худший TRACK-сегмент (макс. ratio внутри
непрерывного сегмента — рост рамки за одно сопровождение).
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


def analyze(run_dir, gt_path):
    gt = {r["frame"]: (r["x"], r["y"], r["w"], r["h"])
          for r in load(gt_path) if r.get("x") is not None}
    tel = load(os.path.join(run_dir, "telemetry.jsonl"))
    ratios, seg_best, seg = [], [], []

    def flush():
        if seg:
            seg_best.append(max(seg))
            seg.clear()

    for t in tel:
        if t["mode"] == "TRACK" and t.get("w") is not None and t["frame_seq"] in gt:
            gw = gt[t["frame_seq"]][2]
            if gw > 0:
                r = t["w"] / gw
                ratios.append(r)
                seg.append(r)
        else:
            flush()
    flush()
    ratios.sort()
    return {
        "n": len(ratios),
        "med": st.median(ratios) if ratios else None,
        "p95": ratios[max(0, int(0.95 * len(ratios)) - 1)] if ratios else None,
        "frac2": sum(1 for r in ratios if r > 2) / len(ratios) if ratios else None,
        "worst_seg": max(seg_best) if seg_best else None,
        "n_segs": len(seg_best),
    }


def main():
    root, gt_dir = sys.argv[1], sys.argv[2]
    print(f"{'ролик':14s} {'кадры':>6s} {'w/wGT мед':>10s} {'p95':>5s} {'>2x':>5s} "
          f"{'худш.сегм':>10s} {'сегм':>5s}")
    for name in sorted(os.listdir(root)):
        sub = os.path.join(root, name, "runs")
        gt_path = os.path.join(gt_dir, name + ".gt.jsonl")
        if not os.path.isdir(sub) or not os.path.exists(gt_path):
            continue
        runs = sorted(os.listdir(sub))
        r = analyze(os.path.join(sub, runs[-1]), gt_path)
        print(f"{name:14s} {r['n']:6d} {r['med']:10.2f} {r['p95']:5.1f} "
              f"{100 * r['frac2']:4.0f}% {r['worst_seg']:9.1f}x {r['n_segs']:5d}")


if __name__ == "__main__":
    main()

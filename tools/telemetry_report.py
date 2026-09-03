#!/usr/bin/env python3
"""Сводка прогона synergy v2 — по каталогу диагностики (ADR-017).

    python tools/telemetry_report.py [путь]        # data/runs/<run> или файл telemetry.jsonl
    python tools/telemetry_report.py run1 run2      # сравнение прогонов

Разделы: сессия (git-хеш, конфиг), режимы/score/тайминги, гистограмма
уверенности сырых детекций с рекомендацией порога, вибропрофиль GMC,
статистика контура наведения, здоровье (RSS/температуры).
"""
import glob
import json
import math
import os
import statistics
import sys
from collections import Counter


def load_jsonl(path):
    rows = []
    try:
        with open(path, encoding="utf-8") as f:
            for line in f:
                line = line.strip()
                if line:
                    try:
                        rows.append(json.loads(line))
                    except json.JSONDecodeError:
                        pass
    except (FileNotFoundError, IsADirectoryError):
        pass
    return rows


def pct(vals, q):
    if not vals:
        return 0.0
    vals = sorted(vals)
    return vals[min(len(vals) - 1, int(len(vals) * q))]


class Run:
    def __init__(self, path):
        if os.path.isdir(path):
            self.dir = path
        else:
            # файл телеметрии (совместимость) или каталог прогонов
            m = glob.glob(os.path.join(path, "runs", "*"))
            if m:
                self.dir = max(m, key=os.path.getmtime)
            else:
                self.dir = None
        d = self.dir or path
        sess = load_jsonl(os.path.join(d, "session.json"))
        self.session = sess[0] if sess else {}
        self.telemetry = load_jsonl(os.path.join(d, "telemetry.jsonl")) or load_jsonl(path)
        self.raw = load_jsonl(os.path.join(d, "raw_detections.jsonl"))
        self.dets = load_jsonl(os.path.join(d, "detections.jsonl"))
        self.cmd = load_jsonl(os.path.join(d, "commander.jsonl"))
        self.gmc = load_jsonl(os.path.join(d, "gmc.jsonl"))
        self.perf = load_jsonl(os.path.join(d, "perf.jsonl"))

    @property
    def name(self):
        return os.path.basename(self.dir) if self.dir else "?"


def summarize(r: Run, verbose=True):
    out = {}
    lines = []
    lines.append(f"== {r.name} ==")
    if r.session:
        lines.append(
            f"сессия: {r.session.get('git_hash','?')} · {str(r.session.get('started','?'))[:19]} "
            f"· борт: {str(r.session.get('board','?'))[:40]}"
        )
    t = r.telemetry
    if t:
        modes = Counter(x["mode"] for x in t)
        total = len(t)
        lines.append(
            "режимы: " + ", ".join(f"{m} {c} ({100*c/total:.0f}%)" for m, c in modes.most_common())
        )
        track = [x for x in t if x["mode"] == "TRACK" and x.get("x") is not None]
        if track:
            sc = [x["score"] for x in track]
            tm = [x["track_ms"] for x in t if x.get("track_ms")]
            e2e = [x["e2e_ms"] for x in t if x.get("e2e_ms") is not None]
            fps = [x["fps"] for x in t if x.get("fps")]
            out["score_med"] = statistics.median(sc)
            out["track_p95"] = pct(tm, 0.95) if tm else 0
            out["e2e_p95"] = pct(e2e, 0.95) if e2e else 0
            out["fps_med"] = statistics.median(fps) if fps else 0
            lines.append(
                f"score: медиана {out['score_med']:.3f} min {min(sc):.3f} | "
                f"track p95 {out['track_p95']:.2f} мс | e2e p95 {out['e2e_p95']:.2f} мс | "
                f"FPS медиана {out['fps_med']:.1f}"
            )
        # ре-захваты
        reacq, lost_at = [], None
        prev = None
        for x in t:
            m = x["mode"]
            if prev == "TRACK" and m in ("ACQUIRE", "LOST"):
                lost_at = x["ts_ms"]
            if lost_at is not None and m == "TRACK":
                reacq.append(x["ts_ms"] - lost_at)
                lost_at = None
            prev = m
        out["reacq_n"] = len(reacq)
        out["reacq_med"] = statistics.median(reacq) if reacq else 0
        lines.append(
            f"повторных захватов: {len(reacq)}"
            + (f", латентность медиана {statistics.median(reacq):.0f} мс" if reacq else "")
        )
    # L2: гистограмма сырых конфиденсов + рекомендация порога
    if r.raw:
        confs = [x["conf"] for x in r.raw if x.get("conf") is not None]
        pos = [c for c in confs if c > 0]
        if pos:
            bins = [0.0, 0.05, 0.1, 0.2, 0.3, 0.4, 0.5, 0.7, 1.01]
            hist = Counter()
            for c in pos:
                for i in range(len(bins) - 1):
                    if bins[i] <= c < bins[i + 1]:
                        hist[f"{bins[i]:.2f}-{bins[i+1]:.2f}"] += 1
                        break
            lines.append(f"сырые детекции: {len(pos)} с conf>0 (из {len(confs)})")
            lines.append("  гистограмма conf: " + " ".join(f"{k}:{v}" for k, v in sorted(hist.items())))
            # рекомендация: порог ниже нижней границы «верхнего кластера»
            sp = sorted(pos)
            gap_thr = None
            for i in range(len(sp) - 1):
                if sp[i + 1] - sp[i] > 0.15 and i > len(sp) * 0.5:
                    gap_thr = sp[i + 1]
                    break
            rec = gap_thr if gap_thr else max(0.05, sp[0])
            out["conf_recommended"] = rec
            lines.append(
                f"  рекомендация порога: >= {rec:.2f} "
                + ("(по разрыву распределения)" if gap_thr else "(минимум наблюдений)")
            )
    # L4: вибропрофиль
    if r.gmc:
        dx = [x["dx"] for x in r.gmc]
        dy = [x["dy"] for x in r.gmc]
        amp = [math.hypot(a, b) for a, b in zip(dx, dy)]
        out["vib_p95"] = pct(amp, 0.95)
        lines.append(
            f"вибрация (GMC): медиана {statistics.median(amp):.1f} px/кадр, "
            f"p95 {out['vib_p95']:.1f}, max {max(amp):.1f}"
        )
    # L3: контур наведения
    if r.cmd:
        ex = [abs(x["err"][0]) for x in r.cmd]
        ey = [abs(x["err"][1]) for x in r.cmd]
        armed = [x for x in r.cmd if x.get("armed")]
        out["err_rms"] = math.sqrt(statistics.mean(e * e for e in ex + ey))
        lines.append(
            f"наведение: {len(r.cmd)} тиков (armed {len(armed)}), "
            f"RMS ошибки {out['err_rms']:.1f} px, "
            f"медиана |err_x| {statistics.median(ex):.0f} / |err_y| {statistics.median(ey):.0f}"
        )
    # L5: здоровье
    if r.perf:
        last = r.perf[-1]
        rss = [x.get("rss_kb") for x in r.perf if x.get("rss_kb")]
        temp = [x.get("soc_c") for x in r.perf if x.get("soc_c")]
        if rss:
            out["rss_end"] = rss[-1]
            lines.append(f"здоровье: RSS {rss[0]//1024}->{rss[-1]//1024} МБ за {len(r.perf)}*5с")
        if temp:
            lines.append(f"  SoC: медиана {statistics.median(temp):.0f}°C max {max(temp):.0f}°C")
        lines.append(
            f"  последний perf: cap[{last.get('cap_ms')}] dec[{last.get('dec_ms')}] "
            f"track[{last.get('track_ms')}] e2e[{last.get('e2e_ms')}]"
        )
    if verbose:
        print("\n".join(lines))
    return out


def compare(a: Run, b: Run):
    sa, sb = summarize(a, verbose=False), summarize(b, verbose=False)
    print(f"== сравнение {a.name} -> {b.name} ==")
    keys = [
        ("score_med", "score медиана", "{:.3f}"),
        ("track_p95", "track p95, мс", "{:.2f}"),
        ("e2e_p95", "e2e p95, мс", "{:.2f}"),
        ("fps_med", "FPS медиана", "{:.1f}"),
        ("reacq_n", "повторных захватов", "{}"),
        ("reacq_med", "латентность ре-захвата, мс", "{:.0f}"),
        ("vib_p95", "вибрация p95, px", "{:.1f}"),
        ("err_rms", "RMS ошибки наведения, px", "{:.1f}"),
        ("conf_recommended", "рекомендованный порог", "{:.2f}"),
        ("rss_end", "RSS в конце, кБ", "{}"),
    ]
    lower_better = {"track_p95", "e2e_p95", "reacq_n", "reacq_med", "vib_p95", "err_rms", "rss_end"}
    for k, title, fmt in keys:
        if k in sa or k in sb:
            va, vb = sa.get(k, None), sb.get(k, None)
            fa = fmt.format(va) if isinstance(va, (int, float)) else (va if va is not None else "—")
            fb = fmt.format(vb) if isinstance(vb, (int, float)) else (vb if vb is not None else "—")
            mark = ""
            if isinstance(va, (int, float)) and isinstance(vb, (int, float)) and va:
                d = (vb - va) / va * 100
                good = (d < 0) == (k in lower_better)
                mark = f"  {'✓' if good else '✗'} {d:+.0f}%"
            print(f"  {title}: {fa} -> {fb}{mark}")


def main():
    args = sys.argv[1:]
    if not args:
        runs = sorted(glob.glob("data/runs/*"))
        if not runs:
            sys.exit("нет data/runs — укажите путь или запустите борд с --diag")
        args = [runs[-1]]
    if len(args) == 2:
        compare(Run(args[0]), Run(args[1]))
    else:
        summarize(Run(args[0]))


if __name__ == "__main__":
    main()

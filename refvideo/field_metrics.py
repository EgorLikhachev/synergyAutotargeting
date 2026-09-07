#!/usr/bin/env python3
"""Метрики прогона replay без GT: покрытие сопровождения, оценка трекера,
распределение режимов. Дополнение к replay_score.py (которому нужен GT).

    python field_metrics.py <каталог-прогона>
"""
import json
import os
import sys


def main(d):
    # телеметрия: ищем jsonl в подкаталоге runs/<ts>/ или самом каталоге
    cands = []
    for root, _, files in os.walk(d):
        cands += [os.path.join(root, f) for f in files if f.endswith(".jsonl")]
    if not cands:
        print("нет .jsonl в", d)
        return 1
    path = max(cands, key=os.path.getmtime)
    modes, scores, errs = {}, [], []
    n = 0
    for line in open(path, encoding="utf-8"):
        try:
            r = json.loads(line)
        except json.JSONDecodeError:
            continue
        n += 1
        modes[r.get("mode", "?")] = modes.get(r.get("mode", "?"), 0) + 1
        if r.get("mode") == "TRACK":
            s = r.get("score")
            if isinstance(s, (int, float)):
                scores.append(float(s))
            # ошибка наведения: отклонение центра цели от центра кадра, px
            w, h = r.get("w"), r.get("h")
            x, y = r.get("x"), r.get("y")
            if None not in (x, y, w, h):
                errs.append(((x + w / 2) - 320, (y + h / 2) - 240))

    def med(v):
        v = sorted(v)
        return v[len(v) // 2] if v else None

    track = modes.get("TRACK", 0)
    print(f"файл: {os.path.basename(path)}  кадров: {n}")
    print("режимы:", " ".join(f"{k}={v} ({100*v//max(n,1)}%)" for k, v in sorted(modes.items())))
    if scores:
        print(f"score TRACK: медиана {med(scores):.2f}  min {min(scores):.2f}")
    if errs:
        ex = [abs(e[0]) for e in errs]
        ey = [abs(e[1]) for e in errs]
        big = sum(1 for e in errs if abs(e[0]) > 40 or abs(e[1]) > 40)
        print(f"|ошибка| от центра, px: медиана x={med(ex):.0f} y={med(ey):.0f}; "
              f"кадров с >40px: {big} ({100*big//max(track,1)}%)")
    print("покрытие сопровождения: {:.0f}%".format(100 * track / max(n, 1)))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1] if len(sys.argv) > 1 else "."))

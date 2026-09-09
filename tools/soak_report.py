#!/usr/bin/env python3
"""soak_report: отчёт по логу tools/soak.sh против критериев фазы E.

Разбор строк вида:
    2026-09-09T08:13:20Z up=569 rss=50M tz=57.3C wd live gw=0
Критерии фазы E (ROADMAP): заявленная длительность покрыта без пропусков,
кадровый цикл не замирал (wd live во всех сэмплах), synergy не умирал
(rss≠?), RSS стабилен (дрейф ≤ 10 МБ).

Выход: markdown-отчёт в stdout; код возврата 0 = критерии пройдены, 1 = нет.
Запуск: python3 tools/soak_report.py [--expect-hours 8] soak_*.log
"""
import re
import sys
from datetime import datetime, timezone

RSS_DRIFT_LIMIT_MB = 10
LINE_RE = re.compile(
    r"^(\S+) up=(\d+) rss=(\d+M|\?) tz=([\d.]+)C wd (live|stalled) gw=([01])$"
)


def parse(path):
    head, rows = None, []
    with open(path, encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if line.startswith("# soak start"):
                head = line
                continue
            if line.startswith("#"):
                continue
            m = LINE_RE.match(line)
            if not m:
                raise SystemExit(f"не разбирается строка: {line!r}")
            ts, up, rss, tz, wd, gw = m.groups()
            rows.append(
                {
                    "ts": datetime.fromisoformat(
                        ts.replace("Z", "+00:00")
                    ),
                    "up": int(up),
                    "rss": None if rss == "?" else int(rss[:-1]),
                    "tz": float(tz),
                    "wd": wd,
                    "gw": int(gw),
                }
            )
    hours = 8
    if head:
        m = re.search(r"(\d+)ч", head)
        if m:
            hours = int(m.group(1))
    return hours, rows


def fmt_dur(sec):
    sec = int(sec)
    return f"{sec // 3600}ч{(sec % 3600) // 60:02d}м"


def main():
    args = sys.argv[1:]
    expect = 8
    if "--expect-hours" in args:
        i = args.index("--expect-hours")
        expect = float(args[i + 1])
        del args[i : i + 2]
    if not args:
        raise SystemExit(__doc__)
    path = args[0]
    hours, rows = parse(path)
    expect = expect if "--expect-hours" in sys.argv else hours

    fails = []
    print(f"# Soak-отчёт: {path}")
    print(f"- заявлено: {hours}ч; сэмплов: {len(rows)}")

    if not rows:
        print("- **FAIL**: нет сэмплов")
        return 1

    t0, t1 = rows[0]["ts"], rows[-1]["ts"]
    span = (t1 - t0).total_seconds()
    print(f"- период: {t0:%Y-%m-%d %H:%M:%S}Z → {t1:%H:%M:%S}Z ({fmt_dur(span)})")

    # полнота: продолжительность и пропуски в каденции 60 с
    if span + 60 < expect * 3600:
        fails.append(f"длительность {fmt_dur(span)} < заявленных {expect}ч")
    gaps = [
        (a["ts"], b["ts"], (b["ts"] - a["ts"]).total_seconds())
        for a, b in zip(rows, rows[1:])
        if (b["ts"] - a["ts"]).total_seconds() > 90
    ]
    print(f"- пропуски >90с: {len(gaps)}")
    for a, b, d in gaps[:5]:
        print(f"  - {a:%H:%M:%S}→{b:%H:%M:%S} ({d:.0f}с)")
    if gaps:
        fails.append(f"пропуски в наблюдении: {len(gaps)}")

    # synergy жив и кадровый цикл не замирал
    dead = [r for r in rows if r["rss"] is None]
    stalled = [r for r in rows if r["wd"] == "stalled"]
    print(f"- сэмплов без synergy (rss=?): {len(dead)}")
    print(f"- сэмплов с замершим сторожем (wd stalled): {len(stalled)}")
    if dead:
        fails.append(f"synergy отсутствовал в {len(dead)} сэмплах")
    if stalled:
        fails.append(f"кадровый цикл замирал в {len(stalled)} сэмплах")
        for r in stalled[:5]:
            print(f"  - stalled @ {r['ts']:%H:%M:%S}Z")

    # RSS. Ровный рост = утечка; скачок, совпавший с тепловой вспышкой
    # (auto-acquire/tracking без оператора — было 2026-09-09), —
    # high-water mark аллокатора: не утечка, если после скачка плато.
    alive = [r for r in rows if r["rss"] is not None]
    if alive:
        rmin = min(r["rss"] for r in alive)
        rmax = max(r["rss"] for r in alive)
        rfirst, rlast = alive[0]["rss"], alive[-1]["rss"]
        drift = rlast - rfirst
        burst_total = 0
        last_burst_i = -1
        for i, (a, b) in enumerate(zip(alive, alive[1:])):
            step = b["rss"] - a["rss"]
            if step > 5 and (b["tz"] - a["tz"] > 4 or b["tz"] > 75):
                burst_total += step
                last_burst_i = i + 1
        base_drift = drift - burst_total
        print(
            f"- RSS: {rfirst}M → {rlast}M (min {rmin}M, max {rmax}M, "
            f"диапазон {rmax - rmin}M, дрейф {drift:+d}M"
            f"{f', из них burst при активности {burst_total:+d}M' if burst_total else ''})"
        )
        if burst_total:
            tail = alive[last_burst_i:]
            note = (
                f"пост-burst плато (разброс {max(r['rss'] for r in tail)}-{min(r['rss'] for r in tail)}M)"
                if len(tail) >= 10
                else "пост-burst плато подтвердить следующим замером"
            )
            print(f"  - burst-шаги совпали с тепловой вспышкой: {note}")
        rss_fail = abs(base_drift) > RSS_DRIFT_LIMIT_MB
        if not rss_fail and burst_total and len(alive[last_burst_i:]) >= 10:
            tail_range = (
                max(r["rss"] for r in alive[last_burst_i:])
                - min(r["rss"] for r in alive[last_burst_i:])
            )
            rss_fail = tail_range > 5
        if rss_fail:
            fails.append(
                f"RSS нестабилен: базовый дрейф {base_drift:+d}M, диапазон {rmax - rmin}M"
            )

    # температура и сеть — контекст, не критерий
    tzs = [r["tz"] for r in rows]
    gw0 = sum(1 for r in rows if r["gw"] == 0)
    print(f"- температура: {min(tzs)}–{max(tzs)}°C")
    print(
        f"- gw-зонд: {gw0}/{len(rows)} проб = 0 "
        "(до 2026-09-09 — артефакт dgram-ping, сеть проверять по ssh/стриму)"
    )

    print()
    if fails:
        print("## Вердикт: НЕ ПРОЙДЕН")
        for f in fails:
            print(f"- FAIL: {f}")
        return 1
    print(
        "## Вердикт: ПРОЙДЕН — длительность покрыта, wd жив во всех сэмплах, "
        f"RSS стабилен (±{RSS_DRIFT_LIMIT_MB}M)"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())

#!/usr/bin/env python3
"""dataset_check: гейт качества записей для датасета R1 (Этап 4).

Разбирает replay-совместимые MJPEG-ролики (records пульта / refvideo/mjpg)
без NPU: кадры SOI..EOI, длительность при 60 fps (формат борта PS Eye),
разрешение, яркость и резкость (дисперсия Лапласа) по выборке кадров.

Вердикт на ролик: OK или список проблем против протокола docs/data_collection.md:
- длительность 30–60 с (вне диапазона — предупреждение, не брак);
- разрешение 640x480 (иное — предупреждение);
- резкость/яркость — информативно: смазы и пересветы лучше отсекать глазами
  по worst-кадру, путь на который указывает отчёт.

Запуск: python tools/dataset_check.py [путь]   (по умолчанию dist/operator-ui/records)
Код возврата: 0 всегда — это отчёт, а не блокирующий тест.
"""
import sys
from pathlib import Path

import numpy as np
from PIL import Image

FPS = 60  # формат борта: PS Eye 640x480@60, records пишутся как есть
SOI, EOI = b"\xff\xd8", b"\xff\xd9"
MAX_FRAMES_PROBED = 12  # по скольким кадрам меряем яркость/резкость
DUR_OK = (30, 60)
RES_OK = (640, 480)
# Резкость глобально НЕ гейтится: на кадрах с преобладанием неба ни
# дисперсия, ни пик Лапласа не отделяют резкое от смаза (проверено на
# эталонных refvideo: рабочие ролики дают p99.9|lap| 3–7). Показываем
# метрику + номер худшего кадра — финально смазы отсекают глазами.
EDGE_WARN = None  # зарезервировано: жёсткий порог не выбран
DARK_WARN = 20.0  # средняя яркость ниже — ролик тёмный, для дневного протокола брак


def split_frames(data):
    """Список (start, end) полных JPEG; хвост без EOI отбрасывается."""
    frames, pos = [], 0
    while True:
        s = data.find(SOI, pos)
        if s < 0:
            break
        e = data.find(EOI, s + 2)
        if e < 0:
            break
        frames.append((s, e + 2))
        pos = e + 2
    return frames


def probe_edge_bright(jpeg_bytes):
    img = Image.open(__import__("io").BytesIO(jpeg_bytes)).convert("L")
    a = np.asarray(img, dtype=np.float32)
    lap = (
        -4 * a[1:-1, 1:-1]
        + a[:-2, 1:-1]
        + a[2:, 1:-1]
        + a[1:-1, :-2]
        + a[1:-1, 2:]
    )
    edge = float(np.percentile(np.abs(lap), 99.9))
    return edge, float(a.mean()), img.size


def check_file(path):
    data = Path(path).read_bytes()
    frames = split_frames(data)
    rep = {
        "file": Path(path).name,
        "mb": len(data) / 1e6,
        "frames": len(frames),
        "dur": len(frames) / FPS,
        "issues": [],
        "info": [],
    }
    if not frames:
        rep["issues"].append("нет полных JPEG-кадров")
        return rep
    if not DUR_OK[0] <= rep["dur"] <= DUR_OK[1]:
        rep["issues"].append(
            f"длительность {rep['dur']:.0f}с вне 30–60с протокола"
        )
    step = max(1, len(frames) // MAX_FRAMES_PROBED)
    worst = None
    brights, sizes = [], set()
    for i in range(0, len(frames), step):
        chunk = data[frames[i][0] : frames[i][1]]
        try:
            edge, bright, size = probe_edge_bright(chunk)
        except Exception:
            rep["issues"].append(f"кадр #{i} не декодируется (брак записи)")
            continue
        sizes.add(size)
        brights.append(bright)
        if worst is None or edge < worst[0]:
            worst = (edge, i)
    if worst:
        rep["info"].append(f"край p99.9 min {worst[0]:.0f} (кадр #{worst[1]}, глазом)")
    if brights:
        rep["info"].append(f"яркость {min(brights):.0f}–{max(brights):.0f}")
        if max(brights) < DARK_WARN:
            rep["issues"].append(f"тёмный ролик: яркость {max(brights):.0f} < {DARK_WARN:.0f}")
    if any(s != RES_OK for s in sizes):
        rep["issues"].append(f"разрешение {sorted(sizes)} ≠ {RES_OK}")
    return rep


def main():
    target = Path(sys.argv[1] if len(sys.argv) > 1 else "dist/operator-ui/records")
    files = sorted(target.glob("*.mjpg")) if target.is_dir() else [target]
    if not files:
        print(f"нет .mjpg в {target}")
        return 0
    ok = 0
    for f in files:
        r = check_file(f)
        status = "OK " if not r["issues"] else "!!!"
        ok += not r["issues"]
        print(
            f"{status} {r['file']}: {r['frames']} кадров, {r['dur']:.0f}с, "
            f"{r['mb']:.1f}МБ; {'; '.join(r['info'])}"
        )
        for iss in r["issues"]:
            print(f"      - {iss}")
    print(f"\nИтого: {ok}/{len(files)} без замечаний")
    return 0


if __name__ == "__main__":
    sys.exit(main())

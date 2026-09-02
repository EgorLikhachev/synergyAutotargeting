#!/usr/bin/env python3
"""Конвертация NanoTrack ONNX → RKNN для RK3588 (фаза C, docs/ROADMAP.md).

Входы tract-бэкенда: NCHW float32, плоскости R→G→B, значения 0..255
(кроcс-проверено с crates/nano-track/src/imgops.rs::to_nchw_f32).
Поэтому RKNN mean=0/std=1 — uint8-вход отображается линейно в те же числа.

Стратегия точности: backbone — int8 (естественные uint8-входы, максимум
скорости), head — fp16 (feature-входы чувствительны к квантованию).

Запуск (WSL Ubuntu, venv ~/rknn-env):
  ~/rknn-env/bin/python /mnt/c/dev/synergyAutotargeting/tools/convert_nanotrack.py

Самопроверка: симуляция RKNN на x86 против onnxruntime (косинусная близость
и max|Δ| по выходам) — грубые ошибки ловятся до борта.
"""
import os
import random
import sys

import numpy as np
from PIL import Image

REPO = os.environ.get("REPO", "/mnt/c/dev/synergyAutotargeting")
MODELS = os.path.join(REPO, "models")
CALIB_MJPEG = os.path.join(REPO, "calib_src.mjpg")
WORK = "/tmp/nt_calib"
N_PAIRS = int(os.environ.get("N_PAIRS", "80"))

random.seed(42)


def split_mjpeg(path, out_dir, limit=160):
    """Разбор контейнера MJPEG (конкатенация JPEG) на кадры."""
    os.makedirs(out_dir, exist_ok=True)
    data = open(path, "rb").read()
    frames, i = [], 0
    while len(frames) < limit:
        i = data.find(b"\xff\xd8\xff", i)
        if i < 0:
            break
        j = data.find(b"\xff\xd9", i) + 2
        if j < 2:
            break
        frames.append(data[i:j])
        i = j
    paths = []
    for n, f in enumerate(frames):
        p = os.path.join(out_dir, f"f{n:04d}.jpg")
        open(p, "wb").write(f)
        paths.append(p)
    return paths


def get_subwindow(img, cx, cy, size, out):
    """Кроп с mean-паддингом как в трекере (imgops::get_subwindow)."""
    w, h = img.size
    c = (size + 1) // 2
    x0, y0 = int(cx) - c, int(cy) - c
    x1, y1 = x0 + size, y0 + size
    left = max(0, -x0)
    top = max(0, -y0)
    right = max(0, x1 - w)
    bottom = max(0, y1 - h)
    x0 += left
    y0 += top
    box = (x0, y0, x0 + size - left - right, y0 + size - top - bottom)
    crop = img.crop(box)
    if left or top or right or bottom:
        arr = np.asarray(crop).astype(np.float32)
        mean = np.asarray(img).mean(axis=(0, 1))
        canvas = np.empty((size, size, 3), dtype=np.float32)
        canvas[:] = mean
        canvas[top : top + crop.height, left : left + crop.width] = arr
        crop = Image.fromarray(canvas.clip(0, 255).astype(np.uint8))
    return crop.resize((out, out), Image.BILINEAR)


def to_nchw_u8(crop):
    arr = np.asarray(crop, dtype=np.uint8)  # HWC, RGB
    return arr.transpose(2, 0, 1)[None]  # 1,3,H,W


def main():
    import onnxruntime as ort

    frames = split_mjpeg(CALIB_MJPEG, os.path.join(WORK, "frames"))
    print(f"кадров калибровки: {len(frames)}")
    imgs = [Image.open(p).convert("RGB") for p in frames]

    # === Кропы 127/255 вокруг случайных «целей» ===
    z_dir, x_dir, zf_dir, xf_dir = (
        os.path.join(WORK, d) for d in ("z", "x", "zf", "xf")
    )
    for d in (z_dir, x_dir, zf_dir, xf_dir):
        os.makedirs(d, exist_ok=True)

    ort_z = ort.InferenceSession(os.path.join(MODELS, "nanotrack_backbone_127.onnx"))
    ort_x = ort.InferenceSession(os.path.join(MODELS, "nanotrack_backbone_sim.onnx"))
    in_z = ort_z.get_inputs()[0].name
    in_x = ort_x.get_inputs()[0].name

    z_list, x_list, pairs = [], [], []
    for n in range(N_PAIRS):
        img = imgs[n % len(imgs)]
        w, h = img.size
        bw = random.randint(40, min(220, w // 3))
        bh = random.randint(40, min(220, h // 3))
        cx = random.randint(bw // 2 + 1, w - bw // 2 - 1)
        cy = random.randint(bh // 2 + 1, h - bh // 2 - 1)
        # шаблон 127 из бокса, поиск 255 из контекста (как init/update)
        crop127 = get_subwindow(img, cx, cy, max(30, int(bw * 1.4)), 127)
        crop255 = get_subwindow(img, cx, cy, max(60, int(bw * 2.8)), 255)
        z = to_nchw_u8(crop127)
        x = to_nchw_u8(crop255)
        zp = os.path.join(z_dir, f"z{n:03d}.npy")
        xp = os.path.join(x_dir, f"x{n:03d}.npy")
        np.save(zp, z.astype(np.float32))
        np.save(xp, x.astype(np.float32))
        z_list.append(zp)
        x_list.append(xp)
        zf = ort_z.run(None, {in_z: z.astype(np.float32)})[0]
        xf = ort_x.run(None, {in_x: x.astype(np.float32)})[0]
        np.save(os.path.join(zf_dir, f"zf{n:03d}.npy"), zf.astype(np.float32))
        np.save(os.path.join(xf_dir, f"xf{n:03d}.npy"), xf.astype(np.float32))
        pairs.append((os.path.join(zf_dir, f"zf{n:03d}.npy"), os.path.join(xf_dir, f"xf{n:03d}.npy")))
    open(os.path.join(WORK, "ds_z.txt"), "w").write("\n".join(z_list) + "\n")
    open(os.path.join(WORK, "ds_x.txt"), "w").write("\n".join(x_list) + "\n")
    open(os.path.join(WORK, "ds_head.txt"), "w").write(
        "\n".join(f"{a} {b}" for a, b in pairs) + "\n"
    )
    print(f"тензоры калибровки: {len(z_list)} кропов, {len(pairs)} пар zf/xf")

    from rknn.api import RKNN

    def build(name, onnx_file, dataset, quantize, check_input=None, algo=None):
        print(f"\n=== {name}: {'int8' if quantize else 'fp16'} ===")
        rk = RKNN(verbose=False)
        # mean/std по фактическому числу каналов каждого входа
        # (у head входы — 48-канальные фичи backbone'ов).
        import onnx as _onnx
        m = _onnx.load(onnx_file)
        chans = []
        for inp in m.graph.input:
            dims = [d.dim_value for d in inp.type.tensor_type.shape.dim]
            chans.append(int(dims[1]) if len(dims) == 4 else 1)
        means = [[0] * c for c in chans]
        stds = [[1] * c for c in chans]
        cfg = dict(
            mean_values=means,
            std_values=stds,
            target_platform="rk3588",
            optimization_level=3,
        )
        if algo:
            # mmse + per-channel: заметно точнее на маленьких моделях
            cfg.update(quantized_algorithm=algo, quantized_method="channel")
        rk.config(**cfg)
        if rk.load_onnx(model=onnx_file) != 0:
            sys.exit(f"{name}: load_onnxошибка")
        if rk.build(do_quantization=quantize, dataset=dataset if quantize else None) != 0:
            sys.exit(f"{name}: buildошибка")
        out = os.path.join(MODELS, name)
        if rk.export_rknn(out) != 0:
            sys.exit(f"{name}: exportошибка")
        print(f"OK → {out} ({os.path.getsize(out)} байт)")

        # Самопроверка: симуляция на x86 против onnxruntime.
        if check_input is not None:
            if rk.init_runtime(target=None) != 0:
                print("(симуляция недоступна — пропуск проверки)")
                return
            npy, sess, inp = check_input
            feed = np.load(npy)
            ref = sess.run(None, {inp: feed})[0]
            got = rk.inference(inputs=[feed.transpose(0, 2, 3, 1)])[0]  # симулятор ждёт NHWC
            a, b = ref.flatten(), np.asarray(got).flatten()
            cos = float(
                np.dot(a, b) / (np.linalg.norm(a) * np.linalg.norm(b) + 1e-9)
            )
            print(f"проверка: cosine={cos:.5f} max|Δ|={np.abs(a - b).max():.4f}")
        rk.release()

    build(
        "nanotrack_backbone_127.rknn",
        os.path.join(MODELS, "nanotrack_backbone_127.onnx"),
        os.path.join(WORK, "ds_z.txt"),
        True,
        (z_list[0], ort_z, in_z),
        "mmse",
    )
    build(
        "nanotrack_backbone_255.rknn",
        os.path.join(MODELS, "nanotrack_backbone_sim.onnx"),
        os.path.join(WORK, "ds_x.txt"),
        True,
        (x_list[0], ort_x, in_x),
        "mmse",
    )
    build(
        "nanotrack_head.rknn",
        os.path.join(MODELS, "nanotrack_head_sim.onnx"),
        os.path.join(WORK, "ds_head.txt"),
        False,  # fp16: feature-входы чувствительны к int8
    )
    print("\nготово: 3 .rknn в models/")


if __name__ == "__main__":
    main()

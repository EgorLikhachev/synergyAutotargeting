#!/usr/bin/env python3
"""Конвертация YOLOv8-детектора ONNX → RKNN int8 (запасной COCO-детектор,
фаза A: валидация пути детекции до прихода эталонного объекта).

Запуск (WSL): ~/rknn-env/bin/python /mnt/c/dev/synergyAutotargeting/tools/convert_detector.py
"""
import os
import sys

import numpy as np
from PIL import Image

REPO = os.environ.get("REPO", "/mnt/c/dev/synergyAutotargeting")
MODELS = os.path.join(REPO, "models")
CALIB_MJPEG = os.path.join(REPO, "calib_src.mjpg")
WORK = "/tmp/det_calib"
INPUT = int(os.environ.get("INPUT", "640"))


JPEG_SOI = bytes([0xFF, 0xD8, 0xFF])
JPEG_EOI = bytes([0xFF, 0xD9])


def split_mjpeg(path, out_dir, limit=120):
    os.makedirs(out_dir, exist_ok=True)
    data = open(path, "rb").read()
    frames, i = [], 0
    while len(frames) < limit:
        i = data.find(JPEG_SOI, i)
        if i < 0:
            break
        j = data.find(JPEG_EOI, i) + 2
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


def main():
    from rknn.api import RKNN

    frames = split_mjpeg(CALIB_MJPEG, os.path.join(WORK, "frames"))
    print(f"кадров калибровки: {len(frames)}")
    ds = os.path.join(WORK, "ds.txt")
    with open(ds, "w") as f:
        for p in frames:
            f.write(p + chr(10))

    rk = RKNN(verbose=False)
    rk.config(
        mean_values=[[0, 0, 0]],
        std_values=[[1, 1, 1]],
        target_platform="rk3588",
        optimization_level=3,
        quantized_algorithm="mmse",
        quantized_method="channel",
    )
    onnx = os.path.join(MODELS, "yolov8n.onnx")
    if rk.load_onnx(model=onnx) != 0:
        sys.exit("load_onnx ошибка")
    if rk.build(do_quantization=True, dataset=ds) != 0:
        sys.exit("build ошибка")
    out = os.path.join(MODELS, "yolov8n_coco.rknn")
    if rk.export_rknn(out) != 0:
        sys.exit("export ошибка")
    print(f"OK → {out} ({os.path.getsize(out)} байт)")
    # Самопроверка симулятором против onnxruntime на первом кадре.
    import onnxruntime as ort

    if rk.init_runtime(target=None) == 0:
        img = np.asarray(Image.open(frames[0]).convert("RGB").resize((INPUT, INPUT)))
        sess = ort.InferenceSession(onnx)
        name = sess.get_inputs()[0].name
        ref = sess.run(None, {name: img[None].astype(np.float32)})
        got = rk.inference(inputs=[img])
        for i, (r, g) in enumerate(zip(ref, got)):
            a, b = r.flatten(), np.asarray(g).flatten()
            if a.shape != b.shape:
                print(f"  выход {i}: форма ref {a.shape} vs rknn {b.shape}")
                continue
            cos = float(np.dot(a, b) / (np.linalg.norm(a) * np.linalg.norm(b) + 1e-9))
            print(f"  выход {i}: cosine={cos:.5f}")
    rk.release()


if __name__ == "__main__":
    main()

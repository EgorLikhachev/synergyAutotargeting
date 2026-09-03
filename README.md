# synergyAutotargeting

A hybrid target-tracking system for the Radxa ROCK 5A (RK3588S): YOLOv8 object
**detection on the NPU every N frames** + NanoTrack **tracking on every frame**,
with live OSD streaming, telemetry, and an aiming-command output (MSP over
UART). The runtime is **100% Rust** — no Python or C++ in the pipeline.

![CI](https://github.com/EgorLikhachev/synergyAutotargeting/actions/workflows/ci.yml/badge.svg)
![License](https://img.shields.io/badge/license-MIT-blue.svg)
![Version](https://img.shields.io/badge/version-0.1.0-orange.svg)
![Rust](https://img.shields.io/badge/rust-1.75%2B-blue?logo=rust)

> Measured on hardware: **60 FPS** pipeline (640×480), tracking **6.2 ms/frame**
> on the NPU, detection **32 ms** on a separate NPU core, end-to-end
> frame→box latency **10.6 ms**. See
> [docs/HARDWARE_TEST_RESULTS.md](docs/HARDWARE_TEST_RESULTS.md).

## Table of contents

- [Prerequisites](#prerequisites)
- [Installation](#installation)
- [Configuration](#configuration)
- [Usage](#usage)
- [Testing](#testing)
- [Project structure](#project-structure)
- [Contributing](#contributing)
- [License](#license)
- [Acknowledgements](#acknowledgements)

## Prerequisites

| Tool | Version | Needed for |
|---|---|---|
| Rust toolchain | 1.75+ (`rustup`) | building, tests |
| Linux (aarch64) | vendor Debian on ROCK 5A | NPU + camera runtime |
| `libudev-dev`, `pkg-config` | apt | `serialport` crate on the board |
| Python 3 | 3.10+ | optional host-side tools (viewer, telemetry report) |
| WSL2 Ubuntu + `gcc-aarch64-linux-gnu` | any | optional: cross-compilation |

No NPU hardware? Everything except real detection/tracking runs on any OS via
the synthetic source (`--synthetic`).

## Installation

```bash
# 1. Clone
git clone https://github.com/EgorLikhachev/synergyAutotargeting.git
cd synergyAutotargeting

# 2. Build and test on your machine (no hardware needed)
cargo test --workspace

# 3. On the ROCK 5A board (aarch64 Linux with librknnrt 2.3.x):
sudo apt install libudev-dev pkg-config
cargo build --release --features npu
cp config.example.toml config.toml   # then edit device/paths if needed

# 4. Run 60 seconds on the real camera + NPU:
./target/release/synergy --duration 60
```

For fast iteration from a dev machine, cross-compile in WSL and deploy in one
command (see `tools/deploy.sh`):

```bash
tools/deploy.sh 192.168.0.224   # build → strip → scp → restart systemd service
```

## Configuration

The app reads `config.toml` (copy from `config.example.toml`). Main sections:

| Section | Keys (defaults) | Meaning |
|---|---|---|
| `[camera]` | `device=/dev/video0`, `width/height=640/480`, `fps=60`, `format="mjpeg"`, `queue_depth=4` | V4L2 capture |
| `[detector]` | `model_path=models/model_5_dynamic_rk3588.rknn`, `input_size=640`, `conf_threshold=0.45`, `nms_threshold=0.45`, `class_thresholds={}` | YOLOv8 NPU detector (per-class thresholds override the global one) |
| `[tracker]` | `backend="tract"` or `"rknn"`, model paths for both backends, `swap_rb=false`, `min_track_score=0.30` | NanoTrack; `rknn` runs on the NPU (~6 ms/frame) |
| `[pipeline]` | `detect_every_n=10`, `iou_confirm=0.30`, `lost_patience=3`, `gmc=false` | hybrid logic; `gmc` enables digital stabilization for hard-mounted cameras |
| `[output]` | `dir="data"`, `snapshot_every=30`, `telemetry=true`, `duration_secs=0` | snapshots (`data/frame_XXXXXX.jpg`), JSONL telemetry |
| `[stream]` | `enabled=false`, `bind="0.0.0.0:8080"`, `push_to=""`, `frame_div=2`, `quality=80`, `record=""`, `record_h264=""` | MJPEG OSD streaming and recording |
| `[commander]` | `enabled=false`, `device="/dev/ttyS6"`, `baud=115200`, `rate_hz=30`, `kp/ki/kd`, `lead_s=0.0`, `swap_axes=false` | aiming loop: pixel error → PID → MSP RC channels over UART |
| `[synthetic]` | `shake_px=0.0` | camera-shake simulation for stabilization testing |

Environment variables:

| Variable | Default | Description |
|---|---|---|
| `RUST_LOG` | `info` | log level (`debug` shows model I/O dims, stream clients, etc.) |

## Usage

```bash
# Local, no hardware: synthetic target, tract backend
cargo run --release -- --synthetic --duration 10

# Board: full stack at 60 FPS (rknn tracker + detector + telemetry)
./target/release/synergy --duration 60

# No target object yet? Phantom target in frame center to exercise the tracker
./target/release/synergy --duration 60 --demo-detect

# Live OSD stream — start a receiver on your PC, then:
python tools/viewer.py                     # listens :9000, browser at :9001
./target/release/synergy --duration 0 --stream-push <pc-ip>:9000

# Record the OSD (M-JPEG, or hardware H.264 on the board)
./target/release/synergy --duration 30 --record /tmp/run.mjpg
./target/release/synergy --duration 30 --record-h264 /tmp/run.mkv

# Summarize a run
python tools/telemetry_report.py data/telemetry.jsonl

# Regression benchmark on the board (compares to the stored baseline)
tools/bench.sh

# Aiming loop in simulation (no UART hardware)
# config.toml: [commander] enabled=true, simulate=true
```

CLI flags: `--config PATH`, `--synthetic`, `--duration SEC`, `--demo-detect`,
`--shake PX`, `--stream`, `--stream-push ADDR`, `--record PATH`,
`--record-h264 PATH`, `--output DIR`, `--diag-nets` (on-board backend check).

## Testing

```bash
cargo test --workspace      # 55+ unit tests (decoder, PID, MSP, stabilization…)
cargo clippy --workspace    # CI enforces zero warnings
```

Hardware-in-the-loop checks (on the board): `tools/bench.sh` for performance
regressions and `./target/release/synergy --diag-nets` for tract↔rknn parity.

## Project structure

```
crates/
  common/      shared types (Frame, BBox, Detection)
  capture/     V4L2 direct-ioctl camera capture (MJPEG→RGB)
  rknn-sys/    Rust FFI to librknnrt (NPU runtime), copy-mode
  detector/    YOLOv8 post-processing: auto layout detect, DFL, NMS
  nano-track/  NanoTrack tracker (tract CPU / RKNN NPU backends),
               GMC stabilization, Kalman, stabilizer
  pipeline/    hybrid logic: detection cadence, reacquire, IoU gates
  streaming/   MJPEG server + push mode (pure std)
  commander/   aiming: PID + slew/deadband + lead predictor, MSP v1 codec
  app/         CLI binary `synergy`: config, OSD, telemetry, recorder
models/        ONNX (CPU) and RKNN (NPU int8) model files
tools/         viewer.py, telemetry_report.py, bench.sh, deploy.sh,
               model conversion scripts, systemd unit
docs/          SDD spec, ADR journal, hardware test results, roadmap (Russian)
ai-context/    session handoff notes for AI assistants
```

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Architecture decisions live in
[docs/sdd/decisions.md](docs/sdd/decisions.md) (ADR journal, Russian);
hardware measurements in [docs/HARDWARE_TEST_RESULTS.md](docs/HARDWARE_TEST_RESULTS.md).

## License

Released under the [MIT License](LICENSE).

## Acknowledgements

- **bkb** project — NanoTrack models, YOLOv8 RKNN model, MSP/UART aiming protocol
- **Autotargeting** project — V4L2 capture crate, SDD/ADR documentation practice
- [OpenCV](https://github.com/opencv/opencv) `TrackerNano` implementation this
  tracker is ported from
- [tract](https://github.com/sonos/tract) (pure-Rust ONNX runtime) and
  [rknn-toolkit2](https://github.com/airockchip/rknn-toolkit2)

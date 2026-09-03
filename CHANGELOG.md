# Changelog

All notable changes to this project are documented in this file.
The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and the project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.1.0] - 2026-09-03

First working release: hybrid detection+tracking on RK3588S hardware,
validated end-to-end on a Radxa ROCK 5A.

### Added

- Hybrid pipeline (variant C): YOLOv8 detection every N frames on NPU core 0 +
  NanoTrack tracking on every frame; IoU confirmation gates, automatic
  reacquisition on loss (measured ~19 ms) [ADR-005].
- `rknn-sys`: Rust FFI to librknnrt (copy-mode; `rknn_context` sized per
  arch, layout-aware input shapes, multi-input inference) [ADR-006, ADR-007].
- `nano-track`: NanoTrack ported 1:1 from OpenCV `TrackerNano` on tract (CPU);
  RKNN int8 backend on NPU (6.2 ms/frame, parity with tract) [ADR-010];
  GMC digital stabilization for hard-mounted cameras [ADR-013].
- `streaming`: MJPEG-over-HTTP server with HTML wrapper + push mode where the
  board connects out to the viewer; M-JPEG and hardware H.264 (mpph264enc)
  recording [ADR-009, ADR-015].
- `commander`: aiming loop ported from the bkb project — MSP v1 SET_RAW_RC
  codec (byte-exact), PID with deadband/slew/anti-windup, axis swap for
  rotated cameras, lead predictor with platform-velocity feedforward;
  validated on a platform simulator to ±30 px [ADR-012].
- Telemetry: per-frame JSONL with mode/score/track_ms/e2e_ms,
  `detections.jsonl`, run summary; `tools/telemetry_report.py`.
- Tools: `viewer.py` (zero-dependency stream receiver), `bench.sh`
  (performance regression baseline), `deploy.sh` (WSL cross-compile +
  one-command deploy), model conversion scripts for rknn-toolkit2.
- Ops: systemd unit with auto-restart; USB autosuspend fix; camera
  corrupted-mode guard (3×3 tiling after fast re-open).
- CI: tests + clippy + aarch64 cross-check on every push.

### Fixed

- `rknn_context` must be u64 on aarch64 (i32 corrupted the stack → SIGSEGV in
  `rknn_init`) [ADR-007].
- YOLOv8 NCHW class count read from the wrong dim (silent worker death).
- `let _ = x;` does not drop values — shutdown deadlock on the detector
  channel [ADR-008].
- Vendor gst: JPEG elements unusable, `filesink location=` must be a separate
  argv token, encoder property is `bps=` [ADR-015].
- `rknn_set_input_shapes`: always attempt, ignore rejection on static models.

### Known limitations

- Single-class detector model (bkb); a COCO fallback path exists but is
  disabled pending a 9-branch export (int8 single-tensor quantization
  collapses class scores) [ADR-014].
- On the vendor kernel, inbound TCP to user-space listeners fails while the
  NPU is active — use push-mode streaming or an SSH tunnel [ADR-009].

[Unreleased]: https://github.com/EgorLikhachev/synergyAutotargeting/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/EgorLikhachev/synergyAutotargeting/releases/tag/v0.1.0

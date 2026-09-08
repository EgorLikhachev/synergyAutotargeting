# Changelog

All notable changes to this project are documented in this file.
The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and the project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Fixed
- FC serial config "not saving" root cause found and fixed: Betaflight 4.4
  `validateAndFixConfig()` silently resets **only the serialConfig group**
  on every boot when `isSerialConfigValid()` fails — MSP is capped at
  `MAX_MSP_PORT_COUNT = 3` (VCP counts and must keep MSP), so "MSP on all
  six UARTs" is invalid and was wiped each reboot while EEPROM itself
  wrote fine (features/craft_name persisted — which made it look like a
  flash/paste problem). Valid config: MSP on VCP + 2 UART.
- Board-recovery lesson locked into docs: on Armbian, overlays go to
  `/boot/armbianEnv.txt` (`overlays=rk3588-uart7-m2`), never to
  extlinux.conf (a Radxa-Debian edit there bricked the boot, 2026-09-05).
- far_loop.sh: the `exec 3<>` + background-writer pattern never actually
  transmitted; replaced with the proven direct-write pattern (`printf >
  DEV` loop + `cat DEV`) — the far-end cable loopback then echoed 29/30
  markers.
- Stream 3x3 grid root cause: planar RGB buffer fed to interleaved JPEG
  encoder — cameras were innocent (ADR-018-a).
- Operator UI no longer exits on torn GRBG/MJPEG frames (skipped with warn).
- Camera unavailability no longer crashes the service into a systemd
  restart loop (129 restarts wedged the kernel/network for hours when
  PS Eye enumerated as video0 while config pointed to video1): in-process
  retry every 3 s + RestartSec=10 + stable udev alias /dev/video-pseye.
- Operator UI: no panic on board frame size != 640x480; stale status
  (incl. armed indicator) cleared on control-channel loss.
- ARM button 2-step confirm with 4 s auto-reset and explicit state label.

### Added
- **FC control over USB proven end-to-end (bench)**: the flight controller
  now hangs on the ROCK 5A's USB-A via its USB-C (VCP, udev alias
  /dev/tty-fc for 0483:5740) — zero FC-side config needed (VCP ships with
  MSP). commander streams SET_RAW_RC (38 B, ~23-30 Hz, throttle 1310 /
  arm-aux 1950 verified by strace); the FC applies the frames (verified by
  direct MSP_RC readback). UART wiring stays a field-phase task.
- Autonomous debugging rig: tools/ui_sim.py (headless operator-UI
  simulator over the real push channels :9000/:9010 with keep-alive pings,
  scripted arm/stop/lock/unlock), tools/fc_rcpeek.sh (MSP_RC peek),
  tools/fc_wpos.sh (commander fd probe); strace write-interception
  methodology documented in CURRENT_STATE.
- Board migrated to **Armbian 26.8.3 trixie vendor 6.1.115** (192.168.0.224
  via router DHCP lease):
  provisioning scripts (tools/armbian_provision.sh, armbian_step2.sh),
  in-tree gspca_ov534 (no out-of-tree build), librknnrt installed manually,
  UART7 overlay via armbianEnv.txt, service autostart, both cameras with
  udev aliases (/dev/video-pseye, /dev/video-arducam).
- FC MSP integration toolchain (PC side over COM4, Configurator must be
  disconnected): msp_query.py (status/rc), fc_cli.py, fc_setports.py
  (apply serial config with acks + **post-reboot verification**, 4 retries),
  fc_finduart.py (permutations of valid UART pairs + `$M>` probe from the
  board), fc_txtest.py (reverse path FC→board via LTM/MAVLink telemetry
  transmitters, port isolation, final MSP config).
- Board-side UART diagnostics (run over ssh, auto stop/start synergy):
  msp_probe_board.sh (MSP_STATUS probe), listen_board.sh (byte counter +
  hex dump), far_loop.sh (far-end cable loopback echo).
- FC identified precisely: GEPRC TAKER F405 BLS 60A V2 stack, FC
  GEP-F405-HD V3, Betaflight 4.4.3 target GEPRCF405; UART/pin map from a
  live resource dump added to the wiring doc along with the proof table
  (board/cable/pins proven end-to-end; FC-side attachment points still
  pending pad labels).
- Sony PS Eye support: out-of-tree ov534/gspca driver build, GRBG Bayer
  format + demosaic, 640x480@60 verified.
- Operator UI: video recording to .mjpg (replay-compatible, durable
  flush/stop), hotkeys Esc/F/R, live window title, unlock tracking button
  (Mode::Idle), clickable recording path + open-folder.
- Safety audit vs GOST/NASA principles (docs/safety_compliance.md).
- Reference-video validation rig: 18 replays on real NPU scored vs GT
  (refvideo/RESULTS.md); tracker multiplies coverage x5-7.
- Flight controller wiring doc GEP-F405-HD V3 -> ROCK 5A UART7
  (docs/wiring_gep_f405.md).

### Changed
- Predecessor project names purged from code/config (ADRs and
  SYNERGY_MAP keep the history); deploy.sh is path-independent (wslpath);
  binary dumps removed from git (~18 MB).

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

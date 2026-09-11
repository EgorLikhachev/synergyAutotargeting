# Changelog

All notable changes to this project are documented in this file.
The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and the project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.2.0] - 2026-09-11

### Added
- Arming-disable flags decoded in the UI (ADR-021 refinement): the exact
  BF 4.4.3 armingDisableFlags table (26 flags, runtime_config.h) now
  lives in commander::msp; the operator panel shows "АРМ-блок FC: …"
  (top-3 blockers + count, ARM_SWITCH suppressed) whenever the FC would
  refuse arming. The empirically found bit 0x80 is actually
  ARMING_DISABLED_THROTTLE — our safe frame carries throttle 1310 >
  min_check, so the bit still works as an RC-stream-alive detector, but
  it also means the FC cannot arm while our safe stream is active (the
  rc-alive semantics hold only at T=1310; documented in ADR-021).
- Anti-hijack detection merge (ADR-023): a confirming detection (IoU ≥
  iou_confirm) re-anchors the tracker immediately as before, but an
  out-of-box detection now requires `re_anchor_streak` (default 2)
  consecutive hits before switching the anchor — a single false positive
  can no longer steal the aim point. Streak-wait frames return the real
  tracker score. `[pipeline] re_anchor_streak = 1` restores the old
  immediate-switch behavior.

### Fixed
- CI test job hung forever on Linux since ADR-019: the sdnotify test
  expected an immediate WATCHDOG=1, but kick() is rate-limited
  (max(WatchdogSec/2, 250 ms)) and sends nothing on the first call — the
  second recv_from blocked eternally (invisible on Windows: cfg(unix)).
  Diagnosed via WSL reproduction + strace; fixed with a 3 s read timeout
  and a 300 ms interval wait. Full Linux suite now runs in seconds.
- Linux-only clippy warnings (invisible on the Windows host) failed the
  CI gate after the hang was fixed: v4l2 too_many_arguments, unused
  Write import in the H.264 spawner, dead fps init in run_camera,
  diag::dir() dead_code. CI green since f17e495.
- GRBG demosaic: the audit's row-based rewrite REVERTED by target-
  hardware measurement (ADR-024): 2.06 ms/frame on A76 vs 0.89 ms for
  the original per-pixel code (LLVM-aarch64 auto-vectorizes the simple
  pattern; the bool-match blocked it). x86 benchmarks are unreliable
  even qualitatively for aarch64 decisions.
- Board validation of the audit pass (2026-09-11): e2e (status) 9→1.6
  ms, dec p95 4.37→1.13 ms, e2e p95 20.8→10.46 ms, RSS −27%, FPS
  60.4/60.6 in both bench modes (incl. the real detector in LOST), 1 h
  express soak passed (flat 50 MB RSS), replay within noise of the R4
  baseline; bench baseline saved on the board.
- CI clippy gate (`-D warnings`) made truthful: 34 accumulated warnings
  fixed — mechanical lints auto-applied (useless conversions/casts,
  derivable `Default` impls, doc-quote markers, `-1` multiplication),
  truly dead code deleted (unused `save_jpeg`/`rgb24_to_nv12` duplicates
  in app; the real converter lives in the capture crate), linux-only
  diagnostics correctly gated (`PerfState` and DiagSink perf/dir APIs are
  consumed only by the linux camera loop — `cfg_attr(not(linux),
  allow(dead_code))` instead of false "dead" on Windows), unwrap-after-
  is_some replaced with `if let … .filter()`, `TickDiag` type alias for
  the L3 log tuple, explicit `allow(too_many_arguments)` on rendering /
  pipeline-pass functions. `cargo clippy --workspace` → 0 warnings,
  `cargo test --workspace` → 69/69.
- Soak `gw=0` mystery solved: the Keenetic gateway does not answer
  unprivileged datagram-socket pings (no suid/caps on /usr/bin/ping), so
  ICMP probes from user context always fail while root/raw (netwatch
  service) and all real traffic succeed — the network was healthy all
  along. soak.sh now probes the gateway over TCP :443.
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
- **8-hour soak passed (phase E criterion, 2026-09-09)**: full coverage
  08:13–16:13 UTC, 481/481 samples, zero gaps, zero watchdog stalls, zero
  process deaths; autostart after a clean morning power-cycle confirmed in
  the wild. RSS flat 50→51M for 7.5 h (+1M — no leak in the steady loop);
  end-of-run +15M step coincided with a thermal flare 62→80°C caused by an
  unattended auto-acquire/tracking engagement (mode=LOST evidence, no
  operator connected) and was retained as an allocator high-water mark
  (66→68M over the next 4 h — plateau, not growth). Pipeline sustained
  60 fps / 9 ms e2e across 2.59M frames in 11.9 h. Temperature flag for
  field tests: 80.4°C peak with tracking on passive cooling.
- Bench-week observability: board-side netwatch (gateway ICMP + local TCP
  probe every 30 s, journal on state change, auto `systemctl restart
  systemd-networkd` after 5 min of ping-alive/TCP-dead) and PC-side probe
  log (`data/netwatch_pc.log`, ssh every 30 min — full soak day: no DOWN).
- Operator UI: FC RC-echo bars (ADR-021) — R/P/T/Y/ARM/A2 progress bars
  under the FC line, real values from MSP_RC (what the FC actually
  applies), ARM channel highlighted red above 1700 µs.
- Tooling: tools/soak_report.py (phase-E verdict from a soak log,
  burst-vs-leak RSS analysis), tools/dataset_check.py (R1 recording
  quality gate: frame integrity/duration/resolution/darkness; sharpness
  is reported, not gated — global edge statistics can't separate blur on
  sky-dominated frames, verified on references), tools/enable_token.sh
  (one-shot ADR-020 token enable for the customer retest).
- ui_sim: fc telemetry now logged (online/rx_ok transitions + final
  channel snapshot) — autonomous ADR-021 checks without the GUI.
- Tiled 2×2 inference for small targets (ADR-022, R2): implemented
  (tiles/crop/remap/merge-NMS + `tiled_lost` gating when no track is
  active), measured on the 9-video day reference set, and **disabled by
  default** — no gain with the current model (det-on-target 162→131,
  track-on-target 1140→916): ×2 resolution is not enough for untrained
  7×4 px targets and the ×4 NPU cost slows reacquisition. Retraining (R1)
  remains the confirmed path; the infrastructure stays tested and ready.
- FC telemetry in the UI status (ADR-021): commander now reads MSP replies
  (reader thread + 2 Hz MSP_STATUS/MSP_RC polling over the same port) and
  publishes `fc {online, rx_ok, flags, ch}` — the operator sees the
  flight controller's actual view (RX echo with real channel values, RC
  visibility bit). The RC-alive bit (0x80 in armingDisableFlags) was
  determined empirically on the bench; verified end-to-end: rx_ok follows
  arm/stop within ~1 s and the echo shows 1310/1950 while streaming.
- Safety barriers (safety_compliance §6.2/§6.3, ADR-019/020): systemd
  watchdog (`Type=notify` + `WatchdogSec=5`, sd_notify on pure std, kick
  from all three frame loops — hang = restart into armed=false) and
  control-channel shared-secret auth (`[control] token` + `"auth"` field
  in every command incl. ping; unauthorized traffic no longer refreshes
  the dead-man timer; UI/ui_sim read `SYNERGY_TOKEN` from env). Verified
  live: watchdog timestamp advances, arm rejected without token / accepted
  with it in 60 ms / plain commands still work with token unset.
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

[Unreleased]: https://github.com/EgorLikhachev/synergyAutotargeting/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/EgorLikhachev/synergyAutotargeting/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/EgorLikhachev/synergyAutotargeting/releases/tag/v0.1.0

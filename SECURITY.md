# Security Policy

## Supported versions

| Version | Supported |
|---|---|
| 0.1.x | yes |

## Reporting a vulnerability

Please do **not** open a public GitHub issue for security problems.

Instead, use one of these channels:

1. GitHub **private vulnerability reporting**: Security tab →
   "Report a vulnerability".
2. Email: <your-email@example.com> (replace with the maintainer's address).

Include what you can of: affected component (crate or tool), reproduction
steps, impact assessment, and any suggested mitigation. You will get an
acknowledgement within 72 hours and a status update at least weekly until
resolution.

## Scope notes

This project controls hardware (camera, NPU, and potentially an RC vehicle via
UART). Please pay extra attention to:

- Anything that could cause unintended actuator commands (MSP/UART output).
- Panics or crashes in the streaming/HTTP path reachable from the network.
- Unsafe FFI code in `crates/rknn-sys`.

## Good faith

We consider research done in good faith and welcome reports. Please avoid
degrading service on shared hardware and give us reasonable time to fix before
any disclosure.

# simconnect-rs

SimConnect over TCP for **Microsoft Flight Simulator 2024 on Linux**. No Windows
SDK, no named pipe, no in-prefix helper — a native Linux process reads telemetry
straight off the wire.

Provides the `simconnect-tcp` crate.

## Status

Working. Extracted from `MSFS_Linux_Manager/crates/msfs-simconnect` as Phase 1
of the HangarRS rebuild, and verified against a running MSFS 2024 under Proton:
378 telemetry updates in 10 seconds, full identity, atmosphere and fuel data.

```bash
cargo run --example telemetry 10        # live telemetry from a running sim
```

Adoption by Headtracking, EFBConnect and the ATC agent is a deliberate
follow-on, not part of that phase.

## Design

- **Zero GUI dependency.** Telemetry is published on a `tokio::sync::watch`
  channel, with an optional wake callback for UIs. The four prior copies of this
  client each depended on `eframe` purely to hold an `egui::Context`, which is
  why none of them could be tested or reused.
- **Protocol details are load-bearing** and carry regression tests: the
  offset-28 `RECV_SIMOBJECT_DATA` layout, one `define_id` per variable (bundling
  makes MSFS 2024 drop the whole group with exception 29), the 280-byte
  `build_open` with `"HK\0"` at offset 261, and exception throttling.
- **The reconnect loop is self-contained.** It does not need a process watcher
  telling it the sim started — which matters more than it sounds, because MSFS
  closes the SimConnect port when you leave a flight while the process keeps
  running. Observed live during development.
- **Staleness is explicit.** The sim stops sending during a pause or a menu
  without closing the socket, so `Telemetry::is_stale` is how a consumer avoids
  broadcasting a frozen position to an EFB.
- **`GPS GROUND TRACK` is registered.** Both prior EFB implementations sent
  `PLANE HEADING DEGREES TRUE` in the XGPS track field, which is wrong by the
  crab angle in any crosswind.

## Licence

MIT

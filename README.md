# HangarRS

The flight-sim toolbox for Linux: add-ons, loadouts, Windows installers,
control bindings, the companion-app prefix, telemetry bridges and a map for
Microsoft Flight Simulator 2024 under Steam and Proton, in one app.

HangarRS attaches to the setup you already have, and it is never required to
fly: close or remove it and the simulator launches exactly as before.

Website: <https://triplejumpstudios.github.io/HangarRS/>

## Download

Get the AppImage from
[GitHub Releases](https://github.com/TripleJumpStudios/HangarRS/releases/latest),
make it executable and run it. No installation is needed. Each release lists a
`SHA256SUMS` file; check it with `sha256sum -c SHA256SUMS`.

Requirements:

- x86_64 Linux with glibc 2.31 or newer (Ubuntu 20.04, Debian 11 and later)
- Microsoft Flight Simulator 2024 installed through Steam with Proton
- `nsenter` from util-linux, to start Windows apps inside the running
  simulator's session

## What it does

| | |
|---|---|
| **Flight** | Start the simulator and your companion apps in order, each when it is ready |
| **Hangar** | Everything installed, filed by kind: Community packages, in-sim add-ons, companion apps and native tools |
| **Loadouts** | Named sets of enabled packages; switching moves only what changed, and nothing is copied or deleted |
| **Install** | Windows installers, scenery folders and ZIP archives, with any replaced copy kept |
| **Controls** | The simulator's input profiles, which live in Steam Cloud and not in the prefix: search, snapshot, compare and print them. Read-only in 1.0 |
| **Map** | Open Little Navmap for maps and flight planning |
| **Connections** | Little Navmap and EFB (ForeFlight, Garmin Pilot) output from one telemetry stream |
| **Activity** | Output from companion apps and installers started through HangarRS |
| **Settings** | Discovered paths, Proton selection and write permissions |
| **Advanced** | Set up and inspect the Proton prefix companion apps run in |

Changes to the Community folder are off until you turn them on in Settings.

## Build from source

```bash
cargo build --workspace --locked
cargo run -p hangar-app            # the binary is `hangar-rs`
cargo test --workspace -- --test-threads=1
```

Needs Rust 1.85 or newer. The only C code is the bundled SQLite, built by
`cargo` with the system C compiler. Release builds target glibc 2.31; see
[`docs/PACKAGING.md`](docs/PACKAGING.md).

The workspace includes [`simconnect-rs/`](simconnect-rs/README.md), the
`simconnect-tcp` crate: SimConnect over TCP, with no Windows SDK and no named
pipe. [`compat/littlenavmap/`](compat/littlenavmap/README.md) prepares a pinned
portable Little Navmap with its SimConnect TCP configuration.

## Changes

See [`CHANGELOG.md`](CHANGELOG.md).

## Licence

MIT, see [`LICENSE`](LICENSE). Third-party notices are in
[`packaging/THIRD_PARTY_LICENSES.txt`](packaging/THIRD_PARTY_LICENSES.txt).

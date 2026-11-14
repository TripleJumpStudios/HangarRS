# Local development packaging

The September 12 package was a **development checkpoint** of the current tested
app, using its stripped dev-profile binary. It is not an optimized release or
a claim of compatibility with older distributions. A plain `cargo build` on this
workstation needs **glibc 2.43 or newer**; the release flow below targets
**glibc 2.31**. Either way the app also needs x86_64 Linux, the desktop's
graphics/Wayland/X11 runtime and native file-dialog portal. No simulator data,
configuration, navigation database or proprietary package is included.

Outputs live in a dated directory under `dist/`: AppImage, an unpacked AppDir,
an AppDir tar archive, `SHA256SUMS` and build notes. Run the AppImage or unpack
the tar archive and run `HangarRS.AppDir/AppRun`. No installation is required.
`--version` exits before discovery, the GUI, telemetry or tile access starts.

Rebuild the local package using pre-downloaded tools:

```sh
cargo build --workspace --locked --offline
python3 packaging/package.py --binary target/debug/hangar-rs --version 0.1.0 --dev \
  --appimagetool /path/to/extracted-appimagetool/AppRun \
  --runtime /path/to/runtime-x86_64
```

## Release flow

Release binaries are linked against glibc 2.31 (Ubuntu 20.04, Debian 11) with
[cargo-zigbuild](https://github.com/rust-cross/cargo-zigbuild), which uses
[zig](https://ziglang.org/download/) as the C compiler and linker. The only C in
the graph is the bundled SQLite. Both tools are unpacked under the lane's
`target/` and put on `PATH` for the one command; nothing is installed. Point
the caches there too so nothing lands in `~/.cache`:

```sh
T=$PWD/target/lane-wave-p2/tools          # zig 0.16.0, cargo-zigbuild 0.23.4
export PATH=$T/zig/zig-x86_64-linux-0.16.0:$T/zigbuild/cargo-zigbuild-x86_64-unknown-linux-gnu:$PATH \
  XDG_CACHE_HOME=$T/cache ZIG_GLOBAL_CACHE_DIR=$T/cache/zig ZIG_LOCAL_CACHE_DIR=$T/cache/zig-local
# Panic locations name source files; keep the builder's paths out of the binary.
export RUSTFLAGS="--remap-path-prefix=$HOME/.cargo=/cargo --remap-path-prefix=$HOME/.rustup=/rustup --remap-path-prefix=$PWD=/build"
nice -n 10 cargo zigbuild -p hangar-app --release --locked --offline \
  --target x86_64-unknown-linux-gnu.2.31
B=target/x86_64-unknown-linux-gnu/release/hangar-rs   # under $CARGO_TARGET_DIR if set
objdump -T $B | grep -o 'GLIBC_[0-9.]*' | sort -uV | tail -1   # must be <= GLIBC_2.31
strings $B | grep -c -e /home/ -e "$HOME"                       # must be 0
python3 packaging/package.py --binary $B --version 1.0.1 \
  --appimagetool /path/to/extracted-appimagetool/AppRun \
  --runtime /path/to/runtime-x86_64
```

The `.2.31` suffix on the target is cargo-zigbuild's glibc pin; the Rust target
is still `x86_64-unknown-linux-gnu`. The one warning, *ignoring deprecated linker
optimization setting '1'*, is zig's linker declining rustc's `-O1` and is
harmless. The first 1.0.1 build this way (2026-10-11) needs at most
`GLIBC_2.30`, links libc, libm, libpthread, libdl and ld-linux only (zig links
the unwinder statically, so no `libgcc_s`), and prints `HangarRS 1.0.1`.
`package.py` reads the floor from the binary with `objdump -T` and writes it
into `BUILD.txt`; it refuses, writing nothing, if no `GLIBC_` version is found.
A plain `cargo build --release` still packages, and `BUILD.txt` then says 2.43.
The remap is set for the one command, not in a committed `.cargo/config`, so CI
builds are unaffected. `package.py` refuses, writing nothing, a binary that still
contains `/home/` or the builder's home directory.

`--version` is required. The script runs the binary's own `--version` first and
exits non-zero, writing nothing, unless that output contains the requested
version as a whole token. The output is
`dist/HangarRS-<version>-<YYYYMMDD-HHMMSS>-x86_64/` (`-dev` follows the version
only with `--dev`). An existing output directory is never overwritten.
Verify with `cd <dir> && sha256sum -c SHA256SUMS`. Extract the appimagetool
AppImage with `--appimage-extract` (no FUSE needed). If no standalone runtime
file is on disk, the runtime is the leading bytes of any earlier AppImage, up to
the offset printed by `<image> --appimage-offset`.

The tools come from the official
[appimagetool](https://github.com/AppImage/appimagetool/releases) and
[type2-runtime](https://github.com/AppImage/type2-runtime/releases) releases.
The runtime is verified before use: `runtime-x86_64` from the `continuous`
release is checked against `runtime-x86_64.sig` and the release's
`signing-pubkey.asc` (key `570C 77AC EA40 C0F1 B758 902C BF96 CCA5 6490 F695`)
with `gpg --verify` in a throwaway `GNUPGHOME`, never the user's keyring. The
2026-09-28 build is 944,632 bytes, SHA256
`156f4bdbde9c52d01814600013e0a273f0118dc2de98975f3c8c63427ec79074`. A verified copy, with its `.sig`, the
signing key and a `SHA256SUMS`, is kept in `packaging/runtime/`; pass
`--runtime packaging/runtime/runtime-x86_64`.
Packaging uses an explicit runtime file and does not download, install,
register a desktop entry, or launch the GUI. Existing output directories are
never overwritten. The small SVG is an initial packaging icon; app/package
artwork polish remains separate.

Not yet verified: GUI startup and desktop integration on a clean machine of the
2.31 baseline.

Sim-session app launching requires the host's `/usr/bin/nsenter` from util-linux
and `/usr/bin/env` plus `/bin/sh`. The host tools are not bundled. The launched
Windows program uses the running simulator's Wine toolchain. Missing tools or
namespace-entry refusal are reported as launch failures.

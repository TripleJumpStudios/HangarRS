#!/usr/bin/env python3
"""Package a built binary locally. Does not download tools, install or launch the GUI."""
import argparse
import datetime
import hashlib
import os
import re
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile

parser = argparse.ArgumentParser()
parser.add_argument("--binary", required=True, type=Path)
parser.add_argument("--appimagetool", required=True, type=Path)
parser.add_argument("--runtime", required=True, type=Path)
parser.add_argument("--version", required=True,
    help="release version, e.g. 1.0.0; must appear in the binary's own --version output")
parser.add_argument("--dev", action="store_true",
    help="development checkpoint: adds -dev to the directory name and BUILD.txt wording")
args = parser.parse_args()
root = Path(__file__).resolve().parent.parent

# Refuse before anything is written: the binary must report the requested version.
reported = subprocess.run([str(args.binary.resolve()), "--version"], capture_output=True, text=True, timeout=30)
if reported.returncode != 0 or not re.search(r"(?<![0-9A-Za-z.])" + re.escape(args.version) + r"(?![0-9A-Za-z])", reported.stdout):
    sys.exit(f"refusing: binary --version reports {reported.stdout.strip()!r} (exit {reported.returncode}), "
             f"not {args.version!r}; nothing written")

# Refuse, too, if the third-party notices no longer match the dependency graph.
notices = subprocess.run([sys.executable, "-I", str(root / "scripts/third_party_licenses.py"), "--check"],
    capture_output=True, text=True, timeout=300)
if notices.returncode != 0:
    sys.exit(f"refusing: packaging/THIRD_PARTY_LICENSES.txt is stale ({notices.stdout.strip()}); "
             "run scripts/third_party_licenses.py; nothing written")

# Refuse a binary that carries the builder's home directory: panic locations embed
# the cargo registry and toolchain paths unless the build remapped them
# (RUSTFLAGS --remap-path-prefix, docs/PACKAGING.md "Release flow").
built = args.binary.read_bytes()
home = os.fsencode(str(Path.home()))
leaks = sorted({needle.decode(errors="replace") for needle in (home, b"/home/")
                if len(needle) > 1 and needle in built})
if leaks:
    sys.exit(f"refusing: the binary contains {', '.join(leaks)} ({built.count(b'/home/')} times /home/); "
             "rebuild with --remap-path-prefix; nothing written")

# The glibc floor is read from the binary itself, so BUILD.txt states what this build needs.
dynamic = subprocess.run(["objdump", "-T", str(args.binary.resolve())], capture_output=True, text=True, timeout=60)
glibc = sorted({tuple(int(part) for part in found.split("."))
                for found in re.findall(r"\bGLIBC_(\d+(?:\.\d+)+)\b", dynamic.stdout)})
if dynamic.returncode != 0 or not glibc:
    sys.exit(f"refusing: objdump -T found no GLIBC_ symbol versions (exit {dynamic.returncode}); nothing written")
glibc_floor = ".".join(str(part) for part in glibc[-1])

APP_ID = "com.triplejumpstudios.HangarRS"

stamp = datetime.datetime.now().strftime("%Y%m%d-%H%M%S")
label = args.version + ("-dev" if args.dev else "")
out = root / "dist" / ("HangarRS-" + label + "-" + stamp + "-x86_64")
out.mkdir(parents=True, exist_ok=False)  # never overwrite an existing directory
appdir = out / "HangarRS.AppDir"
(appdir / "usr/bin").mkdir(parents=True)
binary = appdir / "usr/bin/hangar-rs"
shutil.copy2(args.binary.resolve(), binary)
subprocess.run(["strip", "--strip-debug", str(binary)], check=True)
for name in ("AppRun", "hangar-rs.desktop", "hangar-rs.svg"):
    shutil.copy2(root / "packaging" / name, appdir / name)
(appdir / "AppRun").chmod(0o755)
# AppStream metadata and the desktop entry, where software centres look for them.
metainfo = appdir / "usr/share/metainfo"
metainfo.mkdir(parents=True)
shutil.copy2(root / "packaging" / (APP_ID + ".metainfo.xml"), metainfo / (APP_ID + ".metainfo.xml"))
applications = appdir / "usr/share/applications"
applications.mkdir(parents=True)
shutil.copy2(root / "packaging/hangar-rs.desktop", applications / "hangar-rs.desktop")
share = appdir / "usr/share/doc/hangar-rs"
share.mkdir(parents=True)
for source in [root / "README.md", root / "LICENSE", root / "packaging/THIRD_PARTY_LICENSES.txt",
               root / "docs/PACKAGING.md", *sorted((root / "assets/fonts").glob("LICENSE-*.txt"))]:
    shutil.copy2(source, share / source.name)
subprocess.run([str(appdir / "AppRun"), "--version"], check=True)
image = out / (out.name + ".AppImage")
subprocess.run([str(args.appimagetool.resolve()), "--no-appstream", "--runtime-file", str(args.runtime.resolve()), str(appdir), str(image)],
    check=True, env={**os.environ, "ARCH": "x86_64"})
image.chmod(0o755)
with tarfile.open(out / (out.name + "-AppDir.tar.gz"), "x:gz") as archive:
    archive.add(appdir, arcname="HangarRS.AppDir")
files = [image, out / (out.name + "-AppDir.tar.gz")]
checksums = [hashlib.sha256(path.read_bytes()).hexdigest() + "  " + path.name for path in files]
(out / "SHA256SUMS").write_text("\n".join(checksums) + "\n")
(out / "BUILD.txt").write_text(
    ("Development checkpoint, stripped dev-profile binary.\n" if args.dev else
       f"Release {args.version}, stripped release-profile binary.\n") +
    "AppImage and AppDir archive contain the same binary.\n"
    f"Built on this workstation; glibc {glibc_floor} or newer required (highest GLIBC_ symbol).\n"
    "Clean-machine compatibility and GUI launch remain unverified.\n"
    f"Input binary SHA256: {hashlib.sha256(args.binary.read_bytes()).hexdigest()}\n"
    f"Packaged binary SHA256: {hashlib.sha256(binary.read_bytes()).hexdigest()}\n"
)
print(out)

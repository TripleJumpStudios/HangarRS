#!/usr/bin/env python3
"""Generate packaging/THIRD_PARTY_LICENSES.txt from the resolved cargo graph.

Run as: python3 -I scripts/third_party_licenses.py [--check] [--output PATH]

Packages included: every non-workspace package reachable from a workspace
member through normal or build dependency edges, resolved for
x86_64-unknown-linux-gnu only (the AppImage is the one thing shipped).
Packages reachable only through dev-dependency edges are excluded.

License files, from the package's source dir in ~/.cargo/registry (offline):
  - top-level files whose name starts with LICENSE, LICENCE, COPYING,
    UNLICENSE or NOTICE (case-insensitive), plus the manifest's license-file;
  - every file in a top-level LICENSES/ directory (the REUSE convention);
  - every .txt file in a top-level fonts/ directory (font licenses ship
    beside the font, e.g. epaint_default_fonts' OFL.txt and UFL.txt).

A package with none of those is listed under "No license file shipped" with
its expression and its copyright holders (Cargo `authors`), and the canonical
text of every SPDX licence those packages use is appended once, under
"License texts". MIT is the SPDX template ("<year> <copyright holders>"); the
per-package holder line in the list above fills it in. Each canonical text is
read from a package in the same graph, named in CANONICAL; an ID without an
entry stops the script, so a new licence is noticed rather than skipped.

--check regenerates in memory; exit 1 on any difference or missing file.
"""
import json
import os
import re
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUT = os.path.join(ROOT, "packaging", "THIRD_PARTY_LICENSES.txt")
FONTS = os.path.join(ROOT, "assets", "fonts")
TARGET = "x86_64-unknown-linux-gnu"
PREFIXES = ("LICENSE", "LICENCE", "COPYING", "UNLICENSE", "NOTICE")
BAR = "=" * 78

# SPDX ID -> (package in the graph, file inside its source dir).
CANONICAL = {
    "Apache-2.0": ("enum-map", "LICENSES/Apache-2.0.txt"),
    "CC0-1.0": ("enum-map", "LICENSES/CC0-1.0.txt"),
    "MIT": ("enum-map", "LICENSES/MIT.txt"),
    "OFL-1.1": ("epaint_default_fonts", "fonts/OFL.txt"),
    "Ubuntu-font-1.0": ("epaint_default_fonts", "fonts/UFL.txt"),
}


def read(path):
    with open(path, "rb") as f:
        return f.read().decode("utf-8", "replace").replace("\r\n", "\n").rstrip("\n")


def source_dir(pkg):
    d = os.path.dirname(pkg["manifest_path"])
    if not os.path.isdir(d):
        raise SystemExit("missing source dir for %s %s: %s" % (pkg["name"], pkg["version"], d))
    return d


def license_files(pkg):
    d = source_dir(pkg)
    found = set()
    for n in os.listdir(d):
        p = os.path.join(d, n)
        if n.upper().startswith(PREFIXES) and os.path.isfile(p):
            found.add(n)
    for sub, wanted in (("LICENSES", lambda n: True), ("fonts", lambda n: n.endswith(".txt"))):
        sd = os.path.join(d, sub)
        if os.path.isdir(sd):
            for n in os.listdir(sd):
                if wanted(n) and os.path.isfile(os.path.join(sd, n)):
                    found.add(sub + "/" + n)
    lf = pkg.get("license_file")
    if lf and os.path.isfile(os.path.join(d, lf)):
        found.add(os.path.normpath(lf))
    return d, sorted(found)


def spdx_ids(expression):
    return sorted(set(t for t in re.findall(r"[A-Za-z0-9.+-]+", expression or "")
                      if t not in ("OR", "AND", "WITH")))


def holders(pkg):
    authors = [re.sub(r"\s*<[^>]*>", "", a).strip() for a in pkg.get("authors") or []]
    authors = [a for a in authors if a]
    return ", ".join(authors) if authors else "the %s authors" % pkg["name"]


def packages():
    r = subprocess.run(
        ["cargo", "metadata", "--locked", "--offline", "--format-version", "1",
         "--filter-platform", TARGET],
        cwd=ROOT, capture_output=True, check=True)
    m = json.loads(r.stdout)
    byid = {p["id"]: p for p in m["packages"]}
    nodes = {n["id"]: n for n in m["resolve"]["nodes"]}
    ws = set(m["workspace_members"])
    seen, stack = set(), list(ws)
    while stack:
        i = stack.pop()
        if i in seen:
            continue
        seen.add(i)
        for dep in nodes[i]["deps"]:
            if any(k["kind"] in (None, "build") for k in dep["dep_kinds"]):
                stack.append(dep["pkg"])
    pk = [byid[i] for i in seen if i not in ws]
    pk.sort(key=lambda p: (p["name"], p["version"]))
    return pk


def canonical_text(spdx, pk):
    if spdx not in CANONICAL:
        raise SystemExit("no canonical text configured for %s; add it to CANONICAL" % spdx)
    name, rel = CANONICAL[spdx]
    found = [p for p in pk if p["name"] == name]
    if not found:
        raise SystemExit("%s's canonical text comes from %s, which is not in the graph" % (spdx, name))
    src = found[-1]
    path = os.path.join(source_dir(src), rel)
    if not os.path.isfile(path):
        raise SystemExit("missing canonical text for %s: %s" % (spdx, path))
    return "%s %s, %s" % (src["name"], src["version"], rel), read(path)


def generate():
    pk = packages()
    with_files, without = [], []
    for p in pk:
        d, files = license_files(p)
        (with_files if files else without).append((p, d, files))
    o = ["THIRD-PARTY LICENSES", "",
         "HangarRS bundles the following third-party Rust crates and fonts.",
         "Resolved for %s." % TARGET,
         "Packages: %d" % len(pk), ""]
    for p, d, files in with_files:
        o += [BAR, "%s %s" % (p["name"], p["version"]),
              "License: %s" % (p["license"] or "(none declared)"), BAR]
        for f in files:
            o += ["", "--- %s ---" % f, read(os.path.join(d, f))]
        o.append("")
    o += [BAR, "No license file shipped", BAR,
          "These packages ship no license file in their source. Each is listed with",
          "its declared expression and its copyright holders (Cargo authors); the",
          "text of every licence named here follows under \"License texts\".", ""]
    used = set()
    for p, d, files in without:
        o.append("%s %s: %s" % (p["name"], p["version"], p["license"] or "(none declared)"))
        o.append("    Copyright (c) %s" % holders(p))
        used.update(spdx_ids(p["license"]))
    if not without:
        o.append("(none)")
    o.append("")
    if used:
        o += [BAR, "License texts", BAR,
              "One canonical text per licence used above. For MIT, the copyright",
              "line is the holder line listed with each package.", ""]
        for spdx in sorted(used):
            source, text = canonical_text(spdx, pk)
            o += ["--- %s (from %s) ---" % (spdx, source), text, ""]
    o += [BAR, "Fonts", BAR]
    for n in sorted(os.listdir(FONTS)):
        if n.startswith("LICENSE-") and n.endswith(".txt"):
            o += ["", "--- %s ---" % n, read(os.path.join(FONTS, n))]
    o.append("")
    return "\n".join(o)


def main(argv):
    check = "--check" in argv
    out = OUT
    if "--output" in argv:
        out = argv[argv.index("--output") + 1]
    text = generate()
    if check:
        try:
            cur = open(out, encoding="utf-8").read()
        except OSError as e:
            print("missing: %s" % e)
            return 1
        if cur != text:
            print("%s differs from regenerated output" % out)
            return 1
        print("up to date")
        return 0
    os.makedirs(os.path.dirname(out), exist_ok=True)
    with open(out, "w", encoding="utf-8") as f:
        f.write(text)
    print("wrote %s" % out)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))

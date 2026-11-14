# HangarRS — website text

Source for `index.html`. Feature lines follow the app's tab names as of 1.1.0
and claim only what it does. Controls is described as view, search, snapshot
and export; Bind (controllers only, from 1.1.0) is not claimed until it has
been verified live.

## Tagline

The flight-sim toolbox for Linux.

## Paragraph

HangarRS puts the parts of running Microsoft Flight Simulator 2024 under Steam
and Proton in one app: add-ons, loadouts, Windows installers, control
bindings, the companion-app prefix, telemetry bridges and a map. It attaches
to the setup you already have, and it is never required to fly — close or
remove it and the simulator launches exactly as before.

## Features, one per screen

- **Flight** — start the simulator and your companion apps in order, each when
  it is ready.
- **Hangar** — everything installed, filed by kind: Community packages, in-sim
  add-ons, companion apps and native tools.
- **Loadouts** — named sets of enabled packages; switching moves only what
  changed, and nothing is copied or deleted.
- **Install** — Windows installers, scenery folders and ZIP archives, with any
  replaced copy kept.
- **Controls** — the simulator's input profiles, which live in Steam Cloud and
  not in the prefix: search, snapshot, compare and print them.
- **Map** — open Little Navmap for maps and flight planning.
- **Connections** — Little Navmap and EFB (ForeFlight, Garmin Pilot) output
  from one telemetry stream.
- **Settings** — package changes are off until you turn them on.

## Requirements

- x86_64 Linux with glibc 2.31 or newer (Ubuntu 20.04, Debian 11 and later).
- Microsoft Flight Simulator 2024 installed through Steam with Proton.
- One AppImage; no installation.

## Screenshots

`screenshots/*.png`, 1280×800, rendered headlessly from sample data (no real
machine's paths or add-ons). They are CPU renders of the app's own draw
output, so text antialiasing differs slightly from the window.

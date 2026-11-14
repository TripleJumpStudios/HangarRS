# Changelog

## 1.2.0 — 2026-11-02

### Added

- **Remove a package.** A package's details offer *Remove…*. The sheet names the
  package, its size and the loadouts it is in; *Delete* removes that one folder, and
  the name leaves every loadout. A package linked from a vendor's library loses only
  its link. Needs the Community write setting on and the simulator closed.
- **Uninstall a vendor app.** An app with a Windows uninstall entry (such as Orbx
  Central or Project Airports) offers *Uninstall…*. Its own uninstaller
  runs first; the packages you tick are deleted only if it finishes and the app is
  gone. If you cancel it, it fails, or the app was already missing, nothing is
  deleted. How each package is tied to the vendor is shown, and only firm ties are
  ticked for you.
- **Bind on keyboard rows.** Press the key or combination to bind it. Escape cancels.
  Right-hand Ctrl, Shift or Alt in a combination, and combinations never seen in
  your profiles, are refused; number-pad digits bind as the main keys, with a note.
  Keys are read by position on a US layout: on another layout the key proposed is
  the US key in that place, shown before you confirm.

### Changed

- **Stop and Restart reach the exact process.** HangarRS remembers which copy it
  started, by process number and start time, through a self-update. A process
  number reused by something else is never signalled. With several copies running
  and none known to be this one, Stop now refuses rather than picking one, unless
  exactly one runs in the row's own prefix. Matching is by the exact program name.
- Stop on an app that is restarting after an update is refused; press it again once
  the app is back.
- **Bind no longer overwrites newer changes.** If a profile changed on disk after
  HangarRS read it (for example in the simulator's own Controls menu), Bind writes
  nothing, reads the profile again and asks you to bind once more. This applies to
  joystick and keyboard rows, and to Bind since 1.1.
- Switching *Allow package changes* off while a vendor's uninstaller is open now
  stops the package deletes that would follow it.
- Install, placing a dropped package, deleting a preserved copy and applying a
  loadout now wait while a Remove is running, as Remove already waited for them.
- Authorship is now Triple Jump Studios.
- The Prefix tab's sim-link line no longer spells out the sim's AppData path when
  that folder is missing; it names the folder in one sentence.
- **Security:** the archive reader that unpacks dropped packages is updated to
  `zip` 9.5 (fixes GHSA-m665-v7pm-7grj, GHSA-q7v9-x93c-xqv3 and
  GHSA-8hxf-qr8p-62x4), and the map's TLS library to `rustls` 0.23.46. Building
  from source now needs Rust 1.88.

### Removed

- The Content tools view (Corsten's Content Disabler integration) is removed.

## 1.1.0 — 2026-10-19

### Changed

- **Add-on sets are now called Loadouts.** Saved sets are kept as they are.
- **Flight and Hangar decide before they launch.** Each launch first reads where
  an app is already running. One instance where it belongs is adopted; one in
  another Wine prefix is refused with the reason, and nothing is started;
  several instances, or one whose environment cannot be read, are adopted
  without being called a match. BeyondATC keeps its own simulator-session
  checks. Activity numbers each launch attempt.
- **Flight choices follow an app through a rename.** `preflight.toml` now keys
  placements and order by a stable app ID. An older file is read as before and
  rewritten by ID only when a choice changes; `config.toml` is unchanged.
  HangarRS 1.0.x reading the new file shows default placements.
- **Stop asks a Windows app to close first**, the same as its close button, and
  only then ends it. Little Navmap should no longer report that it did not exit
  properly after a Stop.
- **Controls → Compare** lists binding changes first, by context, with each
  control's name and its before and after. Actions added by a simulator update
  are counted rather than listed.

### Added

- **BeyondATC connection** on its Flight row, read from BeyondATC's own log:
  *Connected* only after it reports the simulator in this run. Other running
  apps say *Connection unverified*.
- **Apps started from Hangar are followed through a self-update restart**, as
  Flight's are: the row says *Updating*, and a second launch waits.
- **Use the installed copy**: the version-drift warning offers to point the app
  at the copy its installer recorded.
- **Check again** and **Retry launch** on a failed Flight step; retry is offered
  only once a fresh check finds nothing running.
- Flight says when MSFS restarted under it.
- **Loadouts from Hangar**: a package's details show the loadouts it is in; a
  click adds or removes it.
- **Loadouts** warns while MSFS is running that a loadout takes effect after a
  restart.

### Fixed

- **Bind in Controls** works again for controllers: it listens only to the
  profile's own device, says at once when that device is unplugged, and brings
  its card into view. Keyboard bindings cannot be captured yet, and say so.
- The test suite no longer depends on MSFS being closed.

## 1.0.1 — 2026-10-10

### Fixed

- **Stop on an app that asks before closing** (seen with BeyondATC). Stop was
  read as a self-update restart, so the app was shown as *Updating* and
  followed instead of closed. A close HangarRS asked for is now final: if the
  app is still open after 15 seconds, HangarRS says it may be asking you to
  confirm in its own window and waits up to 2 minutes, without sending a
  second close or force-killing it.

### Added

- **Release build for older distributions**: the AppImage now needs glibc 2.31
  (Ubuntu 20.04, Debian 11) instead of the build machine's 2.43.
- **Advanced**: a read-only row showing whether the simulator's
  `SimConnect.xml` exposes SimConnect over TCP.
- AppStream metadata and a desktop entry inside the AppImage.
- Third-party licence notices, shipped with the AppImage.

## 1.0.0 — 2026-10-10

First release.

- **Flight**: start the simulator and companion apps in order, each when it is
  ready. Apps that restart themselves to update are followed, shown as
  *Updating* and adopted as the same launch, so Stop and the
  close-on-simulator-exit still reach them. Flight returns to the launch list
  once the simulator and its apps are down.
- **Hangar**: everything installed, filed by kind.
- A warning when the program configured for an app is not the copy its
  installer recorded in the prefix, so an update cannot land in a copy
  HangarRS never starts.
- **Add-on sets**: switch named sets of Community packages by moving only what
  changed.
- **Install**: Windows installers, scenery folders and ZIP archives; replaced
  copies are kept.
- **Controls**: search, snapshot, compare and print the simulator's input
  profiles. Read-only: binding from HangarRS is disabled in 1.0. *Save As*
  replaces an existing file atomically.
- **Map**, **Connections**, **Activity**, **Settings** and **Advanced**.

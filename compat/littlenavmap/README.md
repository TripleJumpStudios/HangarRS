# Little Navmap for local MSFS use

Windows **64-bit 3.0.18**, the current official stable release checked on
September 19, 2026. The portable ZIP SHA256 is pinned in `setup.py` from the
publisher's GitHub release metadata. No executable is run during preparation.

`setup.py prepare` takes `--archive`, `--config`, `--destination`, and `--output`.
It extracts to the review directory, adds app-local SimConnect TCP configuration
for `127.0.0.1:5557`, and seeds separate `MSFS/little_navmap.ini` preferences.
The new companion app entry uses `--settings-path MSFS`; Map, Hangar and
Preflight consequently open the same setup. The existing native Linux/XP12
installation and preferences are untouched.

The INI keys are verified against Little Navmap v3.0.18 `connectdialog.cpp`:
`NavConnect/Remote_tabWidgetConnect=0` selects direct MSFS on Windows and
`Remote_checkBoxConnectOnStartup=2` enables automatic connection. The `Remote_`
prefix is used for the entire dialog, including the direct tab. `MSFS24` is
the scenery database identifier. The bundled SimConnect library opens connection
index 0; it uses the app-local/companion SimConnect.cfg, without Little Navconnect
or HangarRS's LNM bridge. Actual direct telemetry and scenery import still need
a live check; a configured simulator identifier does not mean a database was
already imported.

`setup.py apply --review <output>/review.json` is the separate approved live
operation. It refuses changed inputs, existing destinations and running HangarRS
or Windows LNM. It saves a dated config backup, verifies a staged copy, places
the application by rename and replaces the config atomically. Failed staging is
preserved for inspection. It runs no installer and writes no simulator prefix,
Community folder or XP12 preferences.

Scratch checks: `python3 -B -m unittest discover -s compat/littlenavmap -p 'test_*.py'`.

Sources: [official release](https://github.com/albar965/littlenavmap/releases/tag/v3.0.18),
[direct connection](https://www.littlenavmap.org/manuals/littlenavmap/release/3.0/en/CONNECT.html),
[separate settings](https://www.littlenavmap.org/manuals/littlenavmap/release/3.0/en/COMMANDLINE.html).

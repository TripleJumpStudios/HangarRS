#!/usr/bin/env python3
"""Prepare the official portable MSFS build; apply a reviewed setup separately.

No installer, Wine process, simulator, socket or X-Plane preferences are touched.
Apply adds an independent app directory and backs up HangarRS configuration.
"""
import argparse
import datetime
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import tempfile
import tomllib
import zipfile

ARCHIVE_SHA256 = '4a56358980b253c5876df8c8f7a7bab0873ca006c705eba20571a05e99adbc6f'
ARCHIVE_ROOT = 'LittleNavmap-win64-3.0.18'
SIMCONNECT = '[SimConnect]\nProtocol=IPv4\nAddress=127.0.0.1\nPort=5557\nMaxReceiveSize=41088\nDisableNagle=1\n'
# ConnectDialog uses "Remote_" for ALL tabs. Windows tab 0 is direct MSFS;
# QCheckBox state 2 is checked. Verified against the v3.0.18 source.
SETTINGS = '[NavConnect]\nRemote_tabWidgetConnect=0\nRemote_checkBoxConnectOnStartup=2\n\n[Database]\nSimulator=MSFS24\nLoadingSimulator=MSFS24\n'


def sha(data):
    return hashlib.sha256(data).hexdigest()


def configured(original, destination):
    old = tomllib.loads(original.decode())
    exe = str(destination / 'littlenavmap.exe')
    if old.get('navigation') or any(a['exe'] == exe or a['name'] == 'Little Navmap' for a in old.get('apps', [])):
        raise ValueError('Little Navmap is already configured; select the existing installation in Map')
    entry = {'name': 'Little Navmap', 'exe': exe, 'args': ['--settings-path', 'MSFS'],
             'kind': 'companion', 'enabled': True}
    text = original.decode()
    nav = 'littlenavmap = ' + json.dumps(exe, ensure_ascii=False) + '\n'
    if 'navigation' in old:
        text, count = re.subn(r'(?m)^\[navigation\][ \t]*(?:\n|$)', lambda _: '[navigation]\n' + nav, text)
        if count != 1:
            raise ValueError('Unrecognized navigation table formatting')
    else:
        text += '\n[navigation]\n' + nav
    text += '\n[[apps]]\n'
    # JSON strings, arrays and booleans here are also valid TOML.
    text += ''.join(f'{key} = {json.dumps(value, ensure_ascii=False)}\n' for key, value in entry.items())
    expected = dict(old, apps=old.get('apps', []) + [entry], navigation={'littlenavmap': exe})
    if tomllib.loads(text) != expected:
        raise ValueError('Configuration did not preserve all existing settings')
    return text.encode()


def manifest(root):
    result = {}
    for path in sorted(root.rglob('*')):
        if path.is_symlink():
            raise ValueError('Unexpected symlink in prepared application')
        if path.is_file():
            result[str(path.relative_to(root))] = sha(path.read_bytes())
    return result


def prepare(archive, config, destination, output):
    destination = destination.absolute()
    if destination.exists():
        raise ValueError('Destination exists; refusing replacement')
    if sha(archive.read_bytes()) != ARCHIVE_SHA256:
        raise ValueError('Archive does not match the official Windows 64-bit 3.0.18 SHA256')
    original = config.read_bytes()
    prepared = configured(original, destination)
    output.mkdir(parents=True, exist_ok=False)
    app = output / 'app'
    app.mkdir()
    with zipfile.ZipFile(archive) as zipped:
        for member in zipped.infolist():
            parts = Path(member.filename).parts
            if not parts or parts[0] != ARCHIVE_ROOT or '..' in parts:
                raise ValueError('Unexpected archive layout')
            target = app.joinpath(*parts[1:])
            if member.is_dir():
                target.mkdir(parents=True, exist_ok=True)
            else:
                target.parent.mkdir(parents=True, exist_ok=True)
                with target.open('xb') as file:
                    file.write(zipped.read(member))
    (app / 'MSFS').mkdir()
    (app / 'MSFS/little_navmap.ini').write_text(SETTINGS)
    (app / 'SimConnect.cfg').write_text(SIMCONNECT)
    (output / 'config.toml').write_bytes(prepared)
    review = {'version': '3.0.18', 'config': str(config.absolute()),
              'config_before': sha(original), 'config_after': sha(prepared),
              'destination': str(destination), 'files': manifest(app)}
    (output / 'review.json').write_text(json.dumps(review, indent=2) + '\n')
    print(f'Prepared {len(review["files"])} files with automatic direct MSFS connection.\n{output / "review.json"}')


def ensure_closed():
    for pid in Path('/proc').iterdir():
        if not pid.name.isdigit():
            continue
        try:
            name = (pid / 'cmdline').read_bytes().split(b'\0')[0].replace(b'\\', b'/').split(b'/')[-1].lower()
            if name in (b'hangar-rs', b'littlenavmap.exe'):
                raise ValueError('Close HangarRS and Windows Little Navmap before applying this setup')
        except (FileNotFoundError, PermissionError, ProcessLookupError):
            pass


def apply(review_path):
    review = json.loads(review_path.read_text())
    root = review_path.parent
    config, destination = Path(review['config']), Path(review['destination'])
    ensure_closed()
    original = config.read_bytes()
    prepared = (root / 'config.toml').read_bytes()
    if sha(original) != review['config_before'] or sha(prepared) != review['config_after']:
        raise ValueError('Configuration changed since review')
    if prepared != configured(original, destination) or manifest(root / 'app') != review['files']:
        raise ValueError('Prepared setup changed since review')
    if destination.exists():
        raise ValueError('Destination exists; refusing replacement')
    stamp = datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%d-%H%M%S-%f')
    backup = config.with_name(f'{config.name}.{stamp}.bak')
    with backup.open('xb') as file:
        file.write(original)
        file.flush()
        os.fsync(file.fileno())
    destination.parent.mkdir(parents=True, exist_ok=True)
    # Atomic directory placement; interrupted copies never become launchable.
    # Failed staging remains available for review, never silently deleted.
    stage = Path(tempfile.mkdtemp(prefix='.littlenavmap-', dir=destination.parent))
    shutil.copytree(root / 'app', stage, dirs_exist_ok=True)
    if manifest(stage) != review['files']:
        raise ValueError('Installed copy failed verification')
    ensure_closed()
    if config.read_bytes() != original or destination.exists():
        raise ValueError('Configuration or destination changed while copying')
    stage.rename(destination)
    with tempfile.NamedTemporaryFile(dir=config.parent, prefix='.lnm-config-', delete=False) as file:
        file.write(prepared)
        file.flush()
        os.fsync(file.fileno())
        temporary = Path(file.name)
    shutil.copymode(config, temporary)
    ensure_closed()
    if config.read_bytes() != original:
        raise ValueError('Configuration changed; app is preserved without registration')
    os.replace(temporary, config)
    if config.read_bytes() != prepared:
        raise ValueError('Configuration verification failed; original is preserved')
    print(f'Installed and registered: {destination}\nOriginal configuration: {backup}')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    modes = parser.add_subparsers(dest='mode', required=True)
    prep = modes.add_parser('prepare')
    for name in ['archive', 'config', 'destination', 'output']:
        prep.add_argument('--' + name, type=Path, required=True)
    live = modes.add_parser('apply')
    live.add_argument('--review', type=Path, required=True)
    args = parser.parse_args()
    if args.mode == 'prepare':
        prepare(args.archive, args.config, args.destination, args.output)
    else:
        apply(args.review)

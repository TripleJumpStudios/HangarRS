import json
from pathlib import Path
import tempfile
import tomllib
import unittest
from unittest.mock import patch
import zipfile

import setup


class SetupTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.config = self.root / 'config.toml'
        self.original = b'version = 1\n[[apps]]\nname = "BeyondATC"\nexe = "/BATC.exe"\nkind = "sim-session"\n[bridges.lnm]\nenabled = false\n'
        self.config.write_bytes(self.original)
        self.destination = self.root / 'installed'
        self.archive = self.root / 'fixture.zip'
        with zipfile.ZipFile(self.archive, 'w') as archive:
            archive.writestr(setup.ARCHIVE_ROOT + '/littlenavmap.exe', b'not executed')
        self.hash = patch.object(setup, 'ARCHIVE_SHA256', setup.sha(self.archive.read_bytes()))
        self.hash.start()
        self.addCleanup(self.hash.stop)
        self.closed = patch.object(setup, 'ensure_closed')
        self.closed.start()
        self.addCleanup(self.closed.stop)

    def prepare(self):
        output = self.root / 'prepared'
        setup.prepare(self.archive, self.config, self.destination, output)
        self.assertEqual(self.config.read_bytes(), self.original)
        self.assertFalse(self.destination.exists())
        return output

    def test_direct_connection_and_all_existing_settings_survive(self):
        output = self.prepare()
        config = tomllib.loads((output / 'config.toml').read_text())
        original = tomllib.loads(self.original.decode())
        self.assertEqual(config['apps'][0], original['apps'][0])
        self.assertEqual(config['bridges'], original['bridges'])
        self.assertEqual(config['apps'][1]['args'], ['--settings-path', 'MSFS'])
        self.assertEqual(config['apps'][1]['kind'], 'companion')
        settings = (output / 'app/MSFS/little_navmap.ini').read_text()
        self.assertIn('Remote_tabWidgetConnect=0', settings)
        self.assertIn('Remote_checkBoxConnectOnStartup=2', settings)
        self.assertNotIn('XP12', settings)
        self.assertIn('Port=5557', (output / 'app/SimConnect.cfg').read_text())

    def test_apply_backs_up_config_and_refuses_replacement(self):
        output = self.prepare()
        setup.apply(output / 'review.json')
        self.assertEqual((self.destination / 'littlenavmap.exe').read_bytes(), b'not executed')
        self.assertEqual(list(self.root.glob('config.toml.*.bak'))[0].read_bytes(), self.original)
        with self.assertRaises(ValueError):
            setup.apply(output / 'review.json')

    def test_changed_config_or_prepared_files_refuse_before_writes(self):
        output = self.prepare()
        self.config.write_bytes(self.original + b'\n# changed\n')
        with self.assertRaisesRegex(ValueError, 'Configuration changed'):
            setup.apply(output / 'review.json')
        self.config.write_bytes(self.original)
        (output / 'app/littlenavmap.exe').write_bytes(b'changed')
        with self.assertRaisesRegex(ValueError, 'Prepared setup changed'):
            setup.apply(output / 'review.json')
        self.assertFalse(self.destination.exists())
        self.assertEqual(list(self.root.glob('*.bak')), [])

    def test_empty_navigation_table_is_supported_and_selection_is_preserved(self):
        new = setup.configured(self.original + b'\n[navigation]\n', self.destination)
        self.assertEqual(tomllib.loads(new.decode())['navigation']['littlenavmap'],
                         str(self.destination / 'littlenavmap.exe'))
        with self.assertRaisesRegex(ValueError, 'already configured'):
            setup.configured(new, self.destination)

    def test_unknown_archive_is_refused_before_preparation(self):
        self.archive.write_bytes(b'changed')
        with self.assertRaisesRegex(ValueError, 'SHA256'):
            self.prepare()
        self.assertFalse((self.root / 'prepared').exists())


if __name__ == '__main__':
    unittest.main()

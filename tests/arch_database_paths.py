"""Effective watch paths follow pacman's own DBPath, RootDir and Include parsing."""
import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path


@unittest.skipUnless(shutil.which('pacman-conf'), 'pacman-conf unavailable')
class DatabasePaths(unittest.TestCase):
    def test_effective_paths_and_readonly_defaults(self):
        preset = Path(__file__).resolve().parents[1] / 'presets/plugins/arch'
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            plugin = root / 'plugin'
            (plugin / 'data').mkdir(parents=True)
            (plugin / 'lib').symlink_to(preset / 'lib', target_is_directory=True)
            (plugin / 'default-data').symlink_to(preset / 'default-data', target_is_directory=True)
            env = dict(os.environ, RSTAR_PLUGIN_ROOT=str(plugin))
            operation = ['bash', str(preset / 'query_package_database_paths.sh')]
            default = subprocess.run(operation, env=env, capture_output=True, text=True)
            self.assertEqual(default.returncode, 0, default.stderr)
            self.assertTrue(default.stdout.endswith('/local\n'))
            self.assertEqual(list((plugin / 'data').iterdir()), [])
            for use_root in (False, True):
                with self.subTest(rootdir=use_root):
                    custom = root / 'custom database'
                    custom.mkdir(exist_ok=True)
                    include = plugin / 'data/options.conf'
                    include.write_text(('RootDir' if use_root else 'DBPath') + f' = {custom}\n')
                    (plugin / 'data/pacman.conf.in').write_text(
                        '[options]\nInclude = @PLUGIN_ROOT@/data/options.conf\nArchitecture = auto\n')
                    result = subprocess.run(operation, env=env, capture_output=True, text=True)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    suffix = '/var/lib/pacman/local' if use_root else '/local'
                    self.assertEqual(result.stdout, str(custom).lstrip('/') + suffix + '\n')


if __name__ == '__main__':
    unittest.main()

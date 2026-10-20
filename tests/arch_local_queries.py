"""Local queries must work with read-only persistent data and avoid cache maintenance."""
import os
import subprocess
import tempfile
import unittest
from pathlib import Path

class LocalQueries(unittest.TestCase):
    def test_queries_do_not_prepare_shared_caches_or_keyrings(self):
        preset = Path(__file__).resolve().parents[1] / 'presets/plugins/arch'
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            plugin = root / 'plugin'
            plugin.mkdir()
            (plugin / 'data').mkdir()
            (plugin / 'default-data').symlink_to(preset / 'default-data', target_is_directory=True)
            binary = root / 'pacman'
            binary.write_text('#!/bin/sh\nprintf "%s\\n" "$*" >> "$QUERY_TRACE"\n')
            binary.chmod(0o755)
            env = dict(os.environ, PATH=str(root)+':'+os.environ['PATH'], RSTAR_PLUGIN_ROOT=str(plugin), QUERY_TRACE=str(root/'trace'))
            script = '''source "$1"
prepare_plugin_data() { echo unexpected-config-write >&2; return 71; }
prepare_shared_cache() { echo unexpected-cache-write >&2; return 72; }
prepare_repository_keyrings() { echo unexpected-keyring-write >&2; return 73; }
pacman -Qq
pacman -Qqe
pacman -Ql
'''
            result = subprocess.run(['bash','-eu','-c',script,'query-test',str(preset/'lib/common.sh')],env=env,capture_output=True,text=True)
            self.assertEqual(result.returncode,0,result.stderr)
            lines=(root/'trace').read_text().splitlines()
            self.assertEqual(len(lines),3)
            self.assertEqual(list((plugin/'data').iterdir()), [])
            self.assertTrue(all('--cachedir' not in line for line in lines))

if __name__ == '__main__': unittest.main()

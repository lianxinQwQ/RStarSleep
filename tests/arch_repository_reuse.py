"""Unchanged complete AUR publications should not invoke repo-add again."""
import tempfile
import unittest
from pathlib import Path
from unittest.mock import Mock, patch
import arch_predownload as protocol
arch = protocol.arch

class RepositoryReuse(unittest.TestCase):
    def test_reuses_only_complete_matching_publication(self):
        with tempfile.TemporaryDirectory() as directory:
            p = arch.Preparer.__new__(arch.Preparer)
            p.repo = Path(directory)
            p.refresh_repo = Mock()
            database = p.repo / 'rstarsleep.db.tar.gz'
            artifacts = [dict(name='sample', pkgbase='sample', filename='sample.pkg.tar.zst', sha256='expected', version='1')]
            (p.repo / artifacts[0]['filename']).write_bytes(b'package')
            def command(argv):
                database.write_bytes(b'repository database')
            with patch.object(arch, 'run', side_effect=command) as run:
                p.publish(dict(name='sample'), artifacts)
                self.assertEqual(run.call_count, 1)
                p.publish(dict(name='sample'), artifacts)
                self.assertEqual(run.call_count, 1)
                database.write_bytes(b'changed externally')
                p.publish(dict(name='sample'), artifacts)
                self.assertEqual(run.call_count, 2)
                changed = [dict(artifacts[0], version='2')]
                p.publish(dict(name='sample'), changed)
                self.assertEqual(run.call_count, 3)
                (p.repo / '.aur-publication.json').unlink()
                p.publish(dict(name='sample'), changed)
                self.assertEqual(run.call_count, 4)

if __name__ == '__main__': unittest.main()

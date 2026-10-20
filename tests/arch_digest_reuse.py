"""Invocation-local checksum reuse must notice rewrites and file replacement."""
import hashlib
import tempfile
import unittest
from pathlib import Path
from unittest.mock import Mock
import arch_predownload as protocol


class DigestReuse(unittest.TestCase):
    def test_reads_unchanged_file_once_but_not_rewritten_or_replaced_file(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'package'
            path.write_bytes(b'old package')
            p = protocol.arch.Preparer.__new__(protocol.arch.Preparer)
            p.file_digest = Mock(wraps=protocol.arch.Preparer.file_digest)
            self.assertEqual(p.cached_digest(path), hashlib.sha256(b'old package').hexdigest())
            p.cached_digest(path)
            self.assertEqual(p.file_digest.call_count, 1)
            path.write_bytes(b'new package')
            self.assertEqual(p.cached_digest(path), hashlib.sha256(b'new package').hexdigest())
            replacement = Path(directory) / 'replacement'
            replacement.write_bytes(b'next version')
            replacement.replace(path)
            self.assertEqual(p.cached_digest(path), hashlib.sha256(b'next version').hexdigest())
            self.assertEqual(p.file_digest.call_count, 3)

    def test_writes_during_hashing_are_not_recorded(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'package'
            path.write_bytes(b'old')
            p = protocol.arch.Preparer.__new__(protocol.arch.Preparer)
            def digest(file):
                file.write_bytes(b'new')
                return 'untrusted'
            p.file_digest = digest
            with self.assertRaisesRegex(protocol.arch.PreparationError, 'changed during'):
                p.cached_digest(path)
            self.assertEqual(p.digest_cache, {})

if __name__ == '__main__':
    unittest.main()

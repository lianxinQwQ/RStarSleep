"""Offline coverage for official indexes and persistent AUR metadata snapshots."""
import gzip
import importlib.util
import json
import os
import tempfile
import unittest
from pathlib import Path
from unittest.mock import Mock

path = Path(__file__).resolve().parents[1] / 'presets/plugins/arch/lib/package_metadata.py'
spec = importlib.util.spec_from_file_location('metadata', path)
metadata = importlib.util.module_from_spec(spec)
spec.loader.exec_module(metadata)


def dump(entries):
    return gzip.compress(json.dumps(entries).encode())


def aur(name, base=None, provides=()):
    return dict(Name=name, PackageBase=base or name, Provides=list(provides))


class MetadataTests(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root, self.now = Path(directory.name), 1700000000

    def index(self, payload, official=None):
        self.fetch = Mock(return_value=payload)
        return metadata.MetadataIndex(official or {}, self.root, self.fetch, lambda: self.now)

    def test_official_index_classifies_names_groups_sources_and_versioned_providers(self):
        fields = {'NAME': ['implementation'], 'VERSION': ['2'], 'RSTAR_REPOSITORY': ['extra'],
                  'GROUPS': ['tools'], 'PROVIDES': ['virtual=3', 'bare']}
        index = self.index(b'', {'package': fields})
        satisfies = lambda actual, op, wanted: int(actual) >= int(wanted)
        for target in ('implementation', 'extra/implementation', 'tools', 'extra/tools', 'virtual>=2', 'bare'):
            self.assertTrue(index.official_target(target, satisfies), target)
        for target in ('core/implementation', 'aur/implementation', 'bare>=1', 'virtual>=4', 'tools>=1'):
            self.assertFalse(index.official_target(target, satisfies), target)
        self.fetch.assert_not_called()

    def test_dump_loaded_once_and_exact_name_precedes_providers(self):
        index = self.index(dump([aur('real', 'real-base'), aur('other', provides=['real=2', 'virtual']),
                                aur('split-a', 'split', ['virtual']), aur('split-b', 'split', ['virtual'])]))
        self.assertEqual(index.aur_candidates('real'), ['real-base'])
        self.assertEqual(index.aur_candidates('virtual'), ['other', 'split'])
        self.assertEqual(index.aur_candidates('absent'), [])
        self.fetch.assert_called_once()

    def test_fresh_dump_reused_across_invocations(self):
        index = self.index(dump([aur('pkg')]))
        self.assertEqual(index.aur_candidates('pkg'), ['pkg'])
        again = self.index(b'invalid network response')
        self.assertEqual(again.aur_candidates('pkg'), ['pkg'])
        self.fetch.assert_not_called()

    def test_stale_or_corrupt_dump_requires_fetch_and_failure_falls_back(self):
        index = self.index(dump([aur('old')]))
        index.aur_candidates('old')
        self.now += metadata.AUR_TTL
        again = self.index(b'corrupt')
        self.assertIsNone(again.aur_candidates('old'))
        self.assertIsNone(again.aur_candidates('missing'))
        self.fetch.assert_called_once()
        index.cache.write_bytes(b'not gzip')
        self.now -= metadata.AUR_TTL
        again = self.index(dump([aur('new')]))
        self.assertEqual(again.aur_candidates('new'), ['new'])

    def test_network_error_empty_or_invalid_dump_does_not_establish_absence(self):
        for payload in (dump([]), dump([{'Name': 'partial'}]), dump([aur('../unsafe')]), b'corrupt'):
            index = self.index(payload)
            self.assertIsNone(index.aur_candidates('unknown'))
        index = self.index(b'')
        self.fetch.side_effect = OSError('offline')
        self.assertIsNone(index.aur_candidates('unknown'))

    def test_unrelated_malformed_provider_does_not_disable_the_full_snapshot(self):
        index = self.index(dump([aur('valid', provides=['virtual=2']),
                                aur('broken', provides=['broken, invalid', 'bad~name'])]))
        self.assertEqual(index.aur_candidates('valid'), ['valid'])
        self.assertEqual(index.aur_candidates('virtual'), ['valid'])
        self.assertEqual(index.aur_candidates('broken'), ['broken'])

    def test_aur_discovery_uses_srcinfo_to_check_actual_versions(self):
        entrypoint = path.with_name('predownload.py')
        loader = importlib.util.spec_from_file_location('predownload', entrypoint)
        arch = importlib.util.module_from_spec(loader)
        loader.loader.exec_module(arch)
        p = arch.Preparer.__new__(arch.Preparer)
        p.metadata = self.index(dump([aur('pkg', provides=['virtual=1'])]))
        recipe = arch.srcinfo('pkgbase = pkg\npkgver = 2\npkgrel = 1\npkgname = pkg\nprovides = virtual=2\n', 'x86_64')
        p.load_base = Mock(return_value=recipe)
        p.rpc = Mock(side_effect=AssertionError('unexpected per-name RPC'))
        self.assertEqual(p.aur('virtual')['name'], 'pkg')
        p.rpc.assert_not_called()


if __name__ == '__main__':
    unittest.main()

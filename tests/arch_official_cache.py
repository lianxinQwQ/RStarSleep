"""Trusted official cache reuse, invalidation, and real offline pacman validation."""
import base64
import hashlib
import importlib.util
import io
import json
import os
import shutil
import subprocess
import tarfile
import tempfile
import unittest
from pathlib import Path
from unittest.mock import Mock, patch

path = Path(__file__).resolve().parents[1] / 'presets/plugins/arch/lib/predownload.py'
loader = importlib.util.spec_from_file_location('predownload', path)
arch = importlib.util.module_from_spec(loader)
loader.loader.exec_module(arch)
cache_module = arch.official_cache


def signature(created=1700000000, expires=0):
    hashed = b'\x05\x02' + created.to_bytes(4, 'big')
    if expires:
        hashed += b'\x05\x03' + expires.to_bytes(4, 'big')
    body = b'\x04\x00\x16\x08' + len(hashed).to_bytes(2, 'big') + hashed + b'\x00\x00\x00\x00'
    return b'\xc2' + bytes([len(body)]) + body


class OfficialCacheTests(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root, self.now = Path(directory.name), 1700000001
        self.keys = 'pub:u:255:22:KEY:1700000000:1700100000::u:\nfpr:::::::::FINGERPRINT:\n'
        self.version = 'pacman/libalpm test version 1'
        self.policy = 'PackageRequired\nSigLevel = PackageTrustedOnly'
        self.gpgdir = self.root / 'gnupg'
        self.gpgdir.mkdir()
        (self.gpgdir / 'pubring.kbx').write_bytes(b'public keyring')
        (self.gpgdir / 'trustdb.gpg').write_bytes(b'trust state')
        self.content = b'official package fixture'

    def command(self, args, **kwargs):
        if args[0] == 'pacman-conf':
            output = (f'[options]\nGPGDir = {self.gpgdir}\nSigLevel = {self.policy}\n'
                      '[core]\nServer = https://example.invalid\n')
        elif args[-1] == '--list-keys':
            output = self.keys
        elif args[0] == 'pacman':
            output = self.version
        else:
            output = 'gpg test version 1'
        return subprocess.CompletedProcess(args, 0, output, '')

    def preparer(self):
        p = arch.Preparer.__new__(arch.Preparer)
        p.root = self.root
        p.cache, p.repo, p.work = [self.root / d for d in ('cache', 'repo', 'work')]
        for d in (p.cache, p.repo, p.work):
            d.mkdir(exist_ok=True)
        p.arch, p.files, p.artifacts = 'x86_64', set(), {}
        p.official_conf = self.root / 'pacman.conf'
        p.official_conf.write_text('# command fixture')
        self.filename = 'fixture-1-1-any.pkg.tar.zst'
        self.package = p.cache / self.filename
        p.official_metadata = {self.filename: {'NAME': ['fixture'], 'VERSION': ['1-1'],
            'RSTAR_REPOSITORY': ['core'], 'SHA256SUM': [hashlib.sha256(self.content).hexdigest()],
            'PGPSIG': [base64.b64encode(signature()).decode()]}}
        p.verification = cache_module.OfficialPackageCache(p.repo, p.arch, p.official_conf,
            self.command, p.file_digest, lambda: self.now)
        def download(args, **kwargs):
            self.assertEqual(args[0], '-Sw')
            self.package.write_bytes(self.content)
            return subprocess.CompletedProcess(args, 0)
        p.pacman, p.save_sync = Mock(side_effect=download), Mock()
        return p

    def seed(self):
        p = self.preparer()
        p.fetch([self.filename])
        self.assertFalse(p.verification.path.exists())
        p.finish(['fixture'])
        return p

    def test_first_verification_is_published_only_after_finish_and_reused_next_invocation(self):
        first = self.seed()
        first.pacman.assert_called_once()
        again = self.preparer()
        again.fetch([self.filename])
        again.finish(['fixture'])
        again.pacman.assert_not_called()
        self.assertEqual(again.files, {self.filename})

    def test_saved_download_is_reusable_before_whole_preparation_finishes(self):
        sdk = self.root / 'sdk'
        sdk.mkdir()
        # The transport is exercised against the real SDK in arch_predownload.py.
        (sdk / 'rstar.py').write_text('def save():\n    pass\n')
        first = self.preparer()
        for marker in ('.prepared.json', '.planned.json'):
            (first.repo / marker).write_text('{"old":true}')
        with patch.dict(os.environ, {'RSTAR_CONTROL_SOCKET': 'managed-call', 'RSTAR_SDK_DIR': str(sdk)}):
            first.fetch([self.filename])
        self.assertTrue(first.verification.path.is_file())
        self.assertFalse((first.repo / '.prepared.json').exists())
        self.assertFalse((first.repo / '.planned.json').exists())
        again = self.preparer()
        again.fetch([self.filename])
        again.pacman.assert_not_called()
        self.assertEqual(again.files, {self.filename})

    def test_unrecorded_old_cache_is_verified_and_new_context_invalidates_records(self):
        p = self.preparer()
        self.package.write_bytes(self.content)
        p.fetch([self.filename])
        p.pacman.assert_called_once()
        p.finish(['fixture'])
        for mutate in (lambda: (self.gpgdir / 'trustdb.gpg').write_bytes(b'new trust'),
                       lambda: (self.gpgdir / 'pubring.kbx').write_bytes(b'new keys'),
                       lambda: setattr(self, 'version', 'pacman/libalpm test version 2'),
                       lambda: setattr(self, 'keys', self.keys.replace('pub:u', 'pub:r'))):
            mutate()
            p = self.preparer()
            p.fetch([self.filename])
            p.pacman.assert_called_once()
            p.finish(['fixture'])

    def test_replaced_content_or_changed_repository_identity_requires_verification(self):
        self.seed()
        self.package.write_bytes(b'replaced package')
        p = self.preparer()
        p.fetch([self.filename])
        p.pacman.assert_called_once()
        p.finish(['fixture'])
        p = self.preparer()
        p.official_metadata[self.filename]['VERSION'] = ['2-1']
        p.fetch([self.filename])
        p.pacman.assert_called_once()

    def test_disabled_optional_and_trustall_policies_fail_before_download(self):
        for policy in ('PackageNever', 'PackageOptional\nSigLevel = PackageTrustedOnly',
                       'PackageRequired\nSigLevel = PackageTrustAll'):
            self.policy = policy
            p = self.preparer()
            with self.assertRaisesRegex(arch.PreparationError, 'PackageRequired'):
                p.fetch([self.filename])
            p.pacman.assert_not_called()

    def test_signature_expiration_and_clock_reversal_disable_reuse(self):
        p = self.preparer()
        p.official_metadata[self.filename]['PGPSIG'] = [base64.b64encode(signature(expires=5)).decode()]
        p.fetch([self.filename]); p.finish(['fixture'])
        self.now += 5
        again = self.preparer()
        again.fetch([self.filename])
        again.pacman.assert_called_once()
        self.now = 1699999999
        again = self.preparer()
        again.fetch([self.filename])
        again.pacman.assert_called_once()

    def test_unsupported_signature_can_be_verified_but_is_not_trusted_permanently(self):
        p = self.preparer()
        p.official_metadata[self.filename]['PGPSIG'] = [base64.b64encode(b'unsupported').decode()]
        p.fetch([self.filename]); p.finish(['fixture'])
        self.assertEqual(json.loads(p.verification.path.read_text())['packages'], {})

    def test_detached_signatures_also_allow_reuse(self):
        p = self.preparer()
        p.official_metadata[self.filename].pop('PGPSIG')
        Path(str(self.package) + '.sig').write_bytes(signature())
        p.fetch([self.filename]); p.finish(['fixture'])
        again = self.preparer()
        again.official_metadata[self.filename].pop('PGPSIG')
        again.fetch([self.filename])
        again.pacman.assert_not_called()

    def test_download_failure_or_final_content_change_does_not_publish(self):
        p = self.preparer()
        p.pacman.side_effect = arch.PreparationError('invalid signature')
        with self.assertRaisesRegex(arch.PreparationError, 'invalid signature'):
            p.fetch([self.filename])
        self.assertFalse(p.verification.path.exists())
        self.assertFalse((p.repo / '.prepared.json').exists())
        p = self.preparer()
        p.fetch([self.filename])
        self.package.write_bytes(b'changed after verification')
        with self.assertRaisesRegex(arch.PreparationError, 'checksum mismatch'):
            p.finish(['fixture'])
        p.save_sync.assert_not_called()
        self.assertFalse(p.verification.path.exists())

    def test_trust_change_after_reuse_reverifies_whole_batch_and_unstable_context_fails(self):
        self.seed()
        p = self.preparer()
        p.fetch([self.filename])
        (self.gpgdir / 'trustdb.gpg').write_bytes(b'changed during preparation')
        p.finish(['fixture'])
        p.pacman.assert_called_once()
        original = p.verification.path.read_bytes()
        p = self.preparer()
        p.fetch([self.filename])
        context = dict(p.verification.context, fingerprint='changed')
        p.verification.read_context = Mock(side_effect=[context, dict(context, fingerprint='again')])
        with self.assertRaisesRegex(arch.PreparationError, 'Signing trust changed'):
            p.finish(['fixture'])
        self.assertEqual(p.verification.path.read_bytes(), original)

    def test_bad_or_old_ledger_is_never_used(self):
        p = self.preparer()
        for payload in ('not json', '[]', '{"format":0}', '{"format":1,"architecture":"wrong","packages":{}}'):
            p.verification.path.write_text(payload)
            p = self.preparer()
            p.fetch([self.filename])
            p.pacman.assert_called_once()

    def test_private_keys_are_excluded_and_public_keyboxd_state_invalidates(self):
        p = self.preparer()
        first = p.verification.read_context()
        private = self.gpgdir / 'private-keys-v1.d'
        private.mkdir(); (private / 'key').write_text('private data must not be read')
        self.assertEqual(p.verification.read_context(), first)
        public = self.gpgdir / 'public-keys.d'
        public.mkdir(); (public / 'pubring.db').write_bytes(b'public database')
        self.assertNotEqual(p.verification.read_context(), first)

    def test_signature_packet_expiration_parser_fails_closed(self):
        self.assertEqual(cache_module.signature_deadline(signature()), 0)
        self.assertEqual(cache_module.signature_deadline(signature(expires=5)), 1700000005)
        for payload in (b'', b'corrupt', signature()[:-8], signature() + signature()):
            self.assertIsNone(cache_module.signature_deadline(payload))


@unittest.skipUnless(all(shutil.which(p) for p in ('pacman', 'pacman-conf', 'gpg', 'repo-add', 'fakeroot')),
                     'offline pacman signing tools unavailable')
class RealPacmanVerification(unittest.TestCase):
    def test_signed_download_cache_reuse_and_invalid_packages(self):
        with tempfile.TemporaryDirectory(prefix='rstarsleep-trust-test-') as temporary:
            root = Path(temporary)
            gpgdir = root / 'gnupg'; gpgdir.mkdir(mode=0o700)
            gpg = ['gpg', '--no-options', '--homedir', str(gpgdir), '--batch', '--pinentry-mode',
                   'loopback', '--passphrase', '']
            def command(args):
                return subprocess.run(list(map(str, args)), text=True, capture_output=True, timeout=25)
            generated = command([*gpg, '--quick-generate-key', 'Trust fixture <test@example.invalid>',
                                 'ed25519', 'sign', '1d'])
            self.assertEqual(generated.returncode, 0, generated.stderr)
            self.addCleanup(command, ['gpgconf', '--homedir', gpgdir, '--kill', 'gpg-agent'])
            for case in ('valid', 'invalid_signature', 'missing_signature', 'corrupt_cache'):
                with self.subTest(case=case):
                    base = root / case
                    repo, cache, db = [base / name for name in ('repo', 'cache', 'db')]
                    for directory in (repo, cache, db / 'local', db / 'sync'):
                        directory.mkdir(parents=True)
                    filename = 'fixture-1-1-any.pkg.tar.xz'
                    package = repo / filename
                    with tarfile.open(package, 'w:xz') as archive:
                        content = b'pkgname = fixture\npkgver = 1-1\npkgdesc = fixture\narch = any\nsize = 0\n'
                        member = tarfile.TarInfo('.PKGINFO'); member.size = len(content)
                        archive.addfile(member, io.BytesIO(content))
                    signature_file = Path(str(package) + '.sig')
                    signed = command([*gpg, '--output', signature_file, '--detach-sign', package])
                    self.assertEqual(signed.returncode, 0, signed.stderr)
                    if case == 'invalid_signature':
                        data = signature_file.read_bytes()
                        signature_file.write_bytes(data[:-1] + bytes([data[-1] ^ 1]))
                    elif case == 'missing_signature':
                        signature_file.unlink()
                    built = command(['repo-add', repo / 'fixture.db.tar.gz', package])
                    self.assertEqual(built.returncode, 0, built.stderr)
                    shutil.copyfile(repo / 'fixture.db.tar.gz', db / 'sync/fixture.db')
                    if case == 'corrupt_cache':
                        cached = cache / filename
                        shutil.copyfile(package, cached)
                        contents = cached.read_bytes()
                        cached.write_bytes(contents[:-1] + bytes([contents[-1] ^ 1]))
                    configuration = base / 'pacman.conf'
                    configuration.write_text(f'[options]\nArchitecture = auto\nGPGDir = {gpgdir}\n'
                        f'SigLevel = Required DatabaseOptional\n[fixture]\nServer = {repo.as_uri()}\n')
                    args = ['fakeroot', '--', 'pacman', '--config', configuration, '--dbpath', db,
                            '--cachedir', cache, '--logfile', base / 'pacman.log', '-Sw', '--nodeps',
                            '--nodeps', '--noconfirm', '--', 'fixture=1-1']
                    result = command(args)
                    if case != 'valid':
                        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                        continue
                    self.assertEqual(result.returncode, 0, result.stderr)
                    metadata = {'NAME': ['fixture'], 'VERSION': ['1-1'], 'RSTAR_REPOSITORY': ['fixture'],
                                'SHA256SUM': [hashlib.sha256(package.read_bytes()).hexdigest()],
                                'PGPSIG': [base64.b64encode(signature_file.read_bytes()).decode()]}
                    trusted = cache_module.OfficialPackageCache(repo, 'x86_64', configuration, arch.run,
                                                               arch.Preparer.file_digest)
                    trusted.activate()
                    trusted.verified(filename, metadata, cache / filename)
                    arch.atomic_json(trusted.path, trusted.value())
                    # No file on the server is needed to reuse a verified cache entry.
                    package.unlink(); signature_file.unlink()
                    again = cache_module.OfficialPackageCache(repo, 'x86_64', configuration, arch.run,
                                                             arch.Preparer.file_digest)
                    self.assertTrue(again.reusable(filename, metadata, cache / filename))
                    (cache / filename).write_bytes(b'corrupted later')
                    self.assertFalse(again.reusable(filename, metadata, cache / filename))


if __name__ == '__main__':
    unittest.main()

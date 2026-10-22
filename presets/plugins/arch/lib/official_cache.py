"""Reuse successful pacman verification while content and signing trust stay unchanged."""
import base64
import hashlib
import json
import time
from pathlib import Path


class VerificationError(Exception):
    pass


def signature_deadline(data):
    """Conservatively inspect a binary OpenPGP v4 signature's expiration.

    None means unsupported/malformed: pacman may verify it, but no trust shortcut
    is persisted. Zero means a supported signature without an expiration.
    """
    try:
        offset = 0
        signatures = []
        while offset < len(data):
            header = data[offset]
            offset += 1
            if not header & 128:
                return None
            if header & 64:
                tag = header & 63
                length, offset = _length(data, offset)
            else:
                tag, kind = (header >> 2) & 15, header & 3
                if kind == 3:
                    return None
                width = (1, 2, 4)[kind]
                length = int.from_bytes(data[offset:offset + width], 'big')
                offset += width
            end = offset + length
            if end > len(data):
                return None
            if tag == 2:
                signatures.append(data[offset:end])
            offset = end
        if len(signatures) != 1 or signatures[0][0] != 4:
            return None
        packet = signatures[0]
        size = int.from_bytes(packet[4:6], 'big')
        end = 6 + size
        if end + 2 > len(packet):
            return None
        offset, created, expiration = 6, None, None
        while offset < end:
            size, offset = _length(packet, offset)
            if not size or offset + size > end:
                return None
            kind = packet[offset] & 127
            payload = packet[offset + 1:offset + size]
            if kind in (2, 3):
                if len(payload) != 4:
                    return None
                value = int.from_bytes(payload, 'big')
                if kind == 2:
                    if created is not None:
                        return None
                    created = value
                else:
                    if expiration is not None:
                        return None
                    expiration = value
            offset += size
        if created is None:
            return None
        return created + expiration if expiration else 0
    except (IndexError, ValueError):
        return None


def _length(data, offset):
    first = data[offset]
    offset += 1
    if first < 192:
        return first, offset
    if first < 224:
        return ((first - 192) << 8) + data[offset] + 192, offset + 1
    if first == 255:
        if offset + 4 > len(data):
            raise ValueError('长度数据被截断 / truncated length')
        return int.from_bytes(data[offset:offset + 4], 'big'), offset + 4
    raise ValueError('OpenPGP 数据包不完整 / partial OpenPGP packet')


class OfficialPackageCache:
    def __init__(self, repository, architecture, configuration, run, digest, clock=time.time):
        self.path = Path(repository) / '.official-verifications.json'
        self.architecture, self.configuration = architecture, Path(configuration)
        self.run, self.digest, self.clock = run, digest, clock
        self.context = None
        self.records = {}
        try:
            if not self.path.is_symlink():
                value = json.loads(self.path.read_text())
                if (value.get('format') == 1 and value.get('architecture') == architecture
                        and isinstance(value.get('packages'), dict)):
                    self.records = value['packages']
        except (OSError, ValueError, AttributeError):
            pass

    def read_context(self):
        output = self.run(['pacman-conf', '--config', self.configuration, '--verbose'], capture=True).stdout
        sections, section = {}, None
        for line in output.splitlines():
            if line.startswith('[') and line.endswith(']'):
                section = line[1:-1]
                sections.setdefault(section, {})
            elif section and '=' in line:
                name, value = map(str.strip, line.split('=', 1))
                sections[section].setdefault(name, []).append(value)
        options = sections.get('options', {})
        policies = {}
        for repository, fields in sections.items():
            if repository == 'options':
                continue
            policy = fields.get('SigLevel', options.get('SigLevel', []))
            if ('PackageRequired' not in policy or 'PackageTrustedOnly' not in policy
                    or any(p in policy for p in ('PackageOptional', 'PackageNever', 'PackageTrustAll',
                                                 'PackageTrustUnknown', 'PackageTrustMarginal'))):
                raise VerificationError(f'仓库 {repository} 要求使用 PackageRequired 和 PackageTrustedOnly / Repository {repository} requires PackageRequired and PackageTrustedOnly')
            policies[repository] = policy
        gpgdirs = options.get('GPGDir', [])
        if len(gpgdirs) != 1 or not Path(gpgdirs[0]).is_absolute() or not policies:
            raise VerificationError('无法确定官方仓库的签名配置 / Cannot determine official repository signing configuration')
        directory = Path(gpgdirs[0])
        keys = self.run(['gpg', '--no-options', '--homedir', directory, '--batch',
                         '--with-colons', '--fixed-list-mode', '--with-fingerprint', '--list-keys'],
                        capture=True).stdout
        # Include effective validity and the files governing key/trust semantics.
        # Private keys and random seeds are intentionally excluded.
        files = {}
        for name in ('pubring.kbx', 'pubring.gpg', 'trustdb.gpg', 'gpg.conf', 'common.conf',
                     'tofu.db', 'tofu.db-wal'):
            path = directory / name
            if path.is_file():
                files[name] = self.digest(path)
        public = directory / 'public-keys.d'
        if public.is_dir():
            for path in sorted(public.iterdir()):
                if path.is_file() and not path.is_symlink() and path.name in ('pubring.db', 'pubring.db-wal'):
                    files['public-keys.d/' + path.name] = self.digest(path)
        expires = []
        now = self.clock()
        for line in keys.splitlines():
            fields = line.split(':')
            if fields[0] in ('pub', 'sub') and len(fields) > 6 and fields[6].isdigit():
                deadline = int(fields[6])
                if deadline > now:
                    expires.append(deadline)
        if not hasattr(self, 'versions'):
            self.versions = [self.run([program, '--version'], capture=True).stdout
                             for program in ('pacman', 'gpg')]
        value = dict(architecture=self.architecture, policies=policies, gpgdir=str(directory),
                     keys=keys, files=files, versions=self.versions)
        fingerprint = hashlib.sha256(json.dumps(value, sort_keys=True).encode()).hexdigest()
        return dict(fingerprint=fingerprint, expires=min(expires) if expires else 0)

    def activate(self):
        if self.context is None:
            self.context = self.read_context()
        return self.context

    @staticmethod
    def identity(metadata):
        try:
            name, = metadata['NAME']
            version, = metadata['VERSION']
            repository, = metadata['RSTAR_REPOSITORY']
            digest, = metadata['SHA256SUM']
            if len(digest) != 64 or any(c not in '0123456789abcdef' for c in digest):
                raise ValueError()
            return dict(name=name, version=version, repository=repository, sha256=digest)
        except (KeyError, TypeError, ValueError):
            raise VerificationError('官方软件包缺少仓库标识或 SHA-256 / Official package lacks repository identity or SHA-256') from None

    def reusable(self, filename, metadata, package):
        context = self.activate()
        identity = self.identity(metadata)
        record = self.records.get(filename)
        if (not isinstance(record, dict) or record.get('identity') != identity
                or record.get('context') != context['fingerprint']
                or type(record.get('expires')) is not int or record['expires'] < 0
                or type(record.get('checked_at')) is not int or self.clock() < record['checked_at']
                or (record['expires'] and self.clock() >= record['expires'])
                or not package.is_file() or package.is_symlink()):
            return False
        return self.digest(package) == identity['sha256']

    def verified(self, filename, metadata, package):
        identity = self.identity(metadata)
        if not package.is_file() or package.is_symlink() or self.digest(package) != identity['sha256']:
            raise VerificationError(f'通过 pacman 验证后，官方软件包的校验和不匹配：{filename} / Official package checksum mismatch after pacman verification: {filename}')
        signatures = metadata.get('PGPSIG', [])
        try:
            signature = (base64.b64decode(signatures[0], validate=True) if len(signatures) == 1
                         else Path(str(package) + '.sig').read_bytes() if not signatures else b'')
        except (ValueError, OSError):
            signature = b''
        deadline = signature_deadline(signature)
        self.records.pop(filename, None)
        if deadline is None:
            return
        context = self.activate()
        deadlines = [d for d in (deadline, context['expires']) if d]
        expires = min(deadlines) if deadlines else 0
        if expires and expires <= self.clock():
            raise VerificationError(f'软件包签名在验证期间过期：{filename} / Package signing validity expired during verification: {filename}')
        self.records[filename] = dict(identity=identity, context=context['fingerprint'],
                                     expires=expires, checked_at=int(self.clock()))

    def value(self):
        return dict(format=1, architecture=self.architecture, packages=self.records)

    def expired(self, filename):
        record = self.records.get(filename, {})
        return bool(record.get('expires') and self.clock() >= record['expires'])

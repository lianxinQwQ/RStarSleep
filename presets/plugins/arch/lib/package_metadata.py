"""Local official indexes and one cached AUR metadata snapshot per invocation."""
import gzip
import json
import os
import re
import tempfile
import time
import urllib.request
from pathlib import Path

AUR_URL = 'https://aur.archlinux.org/packages-meta-ext-v1.json.gz'
AUR_TTL = 6 * 60 * 60
NAME = re.compile(r'^[A-Za-z0-9@_+][A-Za-z0-9@._+-]*$')
DEPENDENCY = re.compile(r'^([A-Za-z0-9@_+][A-Za-z0-9@._+-]*)(?:(>=|<=|=|>|<)([^\s]+))?$')


class MetadataIndex:
    def __init__(self, official, cache_dir, fetch=None, clock=time.time, report=None):
        self.cache = Path(cache_dir) / 'aur-packages-meta-ext-v1.json.gz'
        self.fetch, self.clock = fetch or self.download, clock
        self.report = report or (lambda chinese, english: None)
        self.names, self.groups, self.provides = {}, {}, {}
        self.aur_names, self.aur_provides, self.aur_attempted = {}, {}, False
        self.aur_available = False
        for fields in official.values():
            for name in fields.get('NAME', []):
                self.names.setdefault(name, []).append(fields)
            for group in fields.get('GROUPS', []):
                self.groups.setdefault(group, []).append(fields)
            for provide in fields.get('PROVIDES', []):
                match = DEPENDENCY.fullmatch(provide)
                if match:
                    self.provides.setdefault(match[1], []).append((fields, match[3] if match[2] == '=' else None))

    def official_target(self, requirement, satisfies):
        source, value = requirement.split('/', 1) if '/' in requirement else (None, requirement)
        if source == 'aur':
            return False
        match = DEPENDENCY.fullmatch(value)
        if not match:
            return False
        name, operator, version = match.groups()
        def eligible(fields):
            return source is None or fields.get('RSTAR_REPOSITORY') == [source]
        def matches(actual):
            return operator is None or bool(actual and satisfies(actual, operator, version))
        for fields in self.names.get(name, []):
            if eligible(fields) and matches(fields.get('VERSION', [''])[0]):
                return True
        if operator is None and any(eligible(f) for f in self.groups.get(name, [])):
            return True
        return any(eligible(f) and matches(v) for f, v in self.provides.get(name, []))

    @staticmethod
    def download(url):
        request = urllib.request.Request(url, headers={'User-Agent': 'RStarSleep/package-metadata'})
        with urllib.request.urlopen(request, timeout=60) as response:
            return response.read()

    @staticmethod
    def decode(payload):
        records = json.loads(gzip.decompress(payload))
        if not isinstance(records, list) or not records:
            raise ValueError('AUR snapshot must contain a nonempty package list')
        names, provides = {}, {}
        for entry in records:
            if not isinstance(entry, dict):
                raise ValueError('Invalid AUR metadata')
            name, base = entry.get('Name'), entry.get('PackageBase')
            values = entry.get('Provides') or []
            if (not isinstance(name, str) or not NAME.fullmatch(name)
                    or not isinstance(base, str) or not NAME.fullmatch(base)
                    or not isinstance(values, list) or any(not isinstance(v, str) for v in values)):
                raise ValueError('Invalid AUR package identity')
            if name in names:
                raise ValueError('Duplicate AUR package identity')
            names[name] = base
            for value in values:
                match = DEPENDENCY.fullmatch(value)
                # A few unrelated AUR recipes have malformed Provides fields.
                # Keep the full name index usable; selected SRCINFO is still
                # checked strictly before any recipe is built.
                if not match or match[2] not in (None, '='):
                    continue
                provides.setdefault(match[1], set()).add(base)
        return names, provides

    def load_aur(self):
        if self.aur_attempted:
            return
        self.aur_attempted = True
        now = self.clock()
        try:
            if not self.cache.is_symlink() and 0 <= now - self.cache.stat().st_mtime < AUR_TTL:
                started = time.perf_counter()
                self.aur_names, self.aur_provides = self.decode(self.cache.read_bytes())
                self.report(f'AUR 缓存索引耗时：{time.perf_counter() - started:.3f} 秒',
                            f'AUR cached index time: {time.perf_counter() - started:.3f} s')
                self.aur_available = True
                self.report(f'复用 AUR 统一元数据：{len(self.aur_names)} 个包',
                            f'Reuse unified AUR metadata: {len(self.aur_names)} packages')
                return
        except (OSError, ValueError, TypeError, UnicodeError, EOFError):
            pass
        try:
            self.report('下载 AUR 统一元数据并建立索引', 'Download and index unified AUR metadata')
            started = time.perf_counter()
            payload = self.fetch(AUR_URL)
            downloaded = time.perf_counter()
            self.aur_names, self.aur_provides = self.decode(payload)
            self.report(f'AUR 元数据下载 {downloaded - started:.3f} 秒，索引 {time.perf_counter() - downloaded:.3f} 秒',
                        f'AUR metadata download: {downloaded - started:.3f} s; indexing: {time.perf_counter() - downloaded:.3f} s')
        except (OSError, ValueError, TypeError, UnicodeError, EOFError):
            self.report('AUR 统一元数据不可用，回退 RPC 查询', 'Unified AUR metadata unavailable; fall back to RPC')
            return  # An expired or unavailable dump must not prove absence.
        self.aur_available = True
        self.cache.parent.mkdir(parents=True, exist_ok=True)
        with tempfile.NamedTemporaryFile(dir=self.cache.parent, delete=False) as stream:
            temporary = Path(stream.name)
            try:
                stream.write(payload)
                stream.flush()
                os.fsync(stream.fileno())
                os.utime(temporary, (now, now))
                os.replace(temporary, self.cache)
            finally:
                temporary.unlink(missing_ok=True)

    def aur_candidates(self, name):
        self.load_aur()
        if not self.aur_available:
            return None
        if name in self.aur_names:
            return [self.aur_names[name]]
        return sorted(self.aur_provides.get(name, ()))

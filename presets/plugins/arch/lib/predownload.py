#!/usr/bin/env python3
"""Prepare shared official package closures and a locally built AUR repository."""
import base64
import hashlib
import importlib.util
import json
import os
import pwd
import re
import shlex
import shutil
import subprocess
import sys
import tempfile
import tarfile
import urllib.error
import urllib.parse
import urllib.request
from io import BytesIO
from pathlib import Path

NAME = re.compile(r"^[A-Za-z0-9@_+][A-Za-z0-9@._+-]*$")
SPEC = re.compile(r"^([A-Za-z0-9@_+][A-Za-z0-9@._+-]*)(?:(>=|<=|=|>|<)([^\s]+))?$")
REPO = "rstarsleep"
QUERY_MODE = False
CACHE_FORMAT = 1
# Also support importing this entrypoint by filename in protocol tests.
_cache_spec = importlib.util.spec_from_file_location("rstarsleep_aur_cache", Path(__file__).with_name("aur_cache.py"))
aur_cache = importlib.util.module_from_spec(_cache_spec)
_cache_spec.loader.exec_module(aur_cache)


class PreparationError(Exception):
    pass


def log(chinese, english):
    print(f"==> [RStarSleep][arch:predownload] {chinese} / {english}", file=sys.stderr, flush=True)


def run(argv, capture=False, allowed=(0,)):
    result = subprocess.run(list(map(str, argv)), text=True,
                            stdout=subprocess.PIPE if capture else (sys.stderr if QUERY_MODE else None),
                            stderr=subprocess.PIPE if capture else None)
    if result.returncode not in allowed:
        raise PreparationError(f"command failed ({result.returncode}): {shlex.join(list(map(str, argv)))}\n{result.stderr or ''}")
    return result


def target_parts(value):
    if "/" not in value:
        return None, value
    source, name = value.split("/", 1)
    if not NAME.fullmatch(source) or not NAME.fullmatch(name):
        raise PreparationError(f"invalid source-qualified target: {value!r}")
    return source, name


def valid_target(value):
    try:
        source, name = target_parts(value)
        return bool(NAME.fullmatch(name))
    except PreparationError:
        return False


def spec(value):
    _, value = target_parts(value)
    match = SPEC.fullmatch(value)
    if not match:
        raise PreparationError(f"invalid dependency: {value!r}")
    return match.groups()


def satisfies_version(actual, operator, expected):
    if operator is None:
        return True
    comparison = int(run(["vercmp", actual, expected], capture=True).stdout.strip())
    return {"=": comparison == 0, ">": comparison > 0, "<": comparison < 0,
            ">=": comparison >= 0, "<=": comparison <= 0}[operator]


def srcinfo(text, architecture):
    base, packages = {}, {}
    current = base
    for line in text.splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        if "=" not in line:
            raise PreparationError("malformed .SRCINFO")
        key, value = [part.strip() for part in line.split("=", 1)]
        if key == "pkgname":
            if not NAME.fullmatch(value) or value in packages:
                raise PreparationError(f"invalid split package: {value}")
            current = packages.setdefault(value, {})
        current.setdefault(key, []).append(value)
    if len(base.get("pkgbase", [])) != 1 or not packages:
        raise PreparationError("incomplete .SRCINFO")
    name = base["pkgbase"][0]
    if not NAME.fullmatch(name):
        raise PreparationError("invalid pkgbase")

    def values(fields, key):
        return [v for v in fields.get(key, []) + fields.get(f"{key}_{architecture}", []) if v]

    def inherited(fields, key):
        result = []
        for field in (key, f"{key}_{architecture}"):
            result.extend(v for v in (fields[field] if field in fields else base.get(field, [])) if v)
        return result

    runtime, build, provides, runtime_by_package, conflicts = [], values(base, "makedepends") + values(base, "checkdepends"), {}, {}, {}
    for package, fields in packages.items():
        arches = fields.get("arch", base.get("arch", []))
        if arches and "any" not in arches and architecture not in arches:
            raise PreparationError(f"{package} does not support {architecture}")
        runtime_by_package[package] = inherited(fields, "depends")
        runtime.extend(runtime_by_package[package])
        build.extend(v for v in values(fields, "makedepends") + values(fields, "checkdepends") if v)
        provides[package] = inherited(fields, "provides")
        conflicts[package] = inherited(fields, "conflicts")
    for dependency in runtime + build:
        spec(dependency)
    version = "{}-{}".format(base.get("pkgver", [""])[0], base.get("pkgrel", [""])[0])
    if base.get("epoch", ["0"])[0] != "0":
        version = base["epoch"][0] + ":" + version
    sources = values(base, "source")
    unchecked = set()
    for suffix in ("", f"_{architecture}"):
        entries = base.get("source" + suffix, [])
        checksums = [base[key + suffix] for key in ("md5sums", "sha1sums", "sha224sums", "sha256sums", "sha384sums", "sha512sums", "b2sums")
                     if key + suffix in base]
        for index, source in enumerate(entries):
            if not any(index < len(sums) and sums[index] != "SKIP" for sums in checksums):
                unchecked.add(source)
    return dict(name=name, packages=packages, version=version, runtime_by_package=runtime_by_package,
                sources=sources, unchecked_sources=sorted(unchecked),
                runtime=list(dict.fromkeys(runtime)),
                build=list(dict.fromkeys(build)), build_runtime=[v for v in values(base, "depends") if v], provides=provides, conflicts=conflicts, keys=base.get("validpgpkeys", []))


def atomic_copy(source, destination):
    destination = Path(destination)
    with tempfile.NamedTemporaryFile(dir=destination.parent, delete=False) as stream:
        temporary = Path(stream.name)
        try:
            with Path(source).open("rb") as src:
                shutil.copyfileobj(src, stream)
            stream.flush()
            # Packages and repository metadata must be readable by pacman's DownloadUser.
            os.fchmod(stream.fileno(), 0o644)
            os.fsync(stream.fileno())
            os.replace(temporary, destination)
        finally:
            temporary.unlink(missing_ok=True)


def atomic_json(path, value):
    with tempfile.NamedTemporaryFile(mode="w", dir=path.parent, delete=False) as stream:
        temporary = Path(stream.name)
        try:
            json.dump(value, stream, indent=2, sort_keys=True)
            stream.write("\n")
            stream.flush()
            os.fsync(stream.fileno())
            os.replace(temporary, path)
        finally:
            temporary.unlink(missing_ok=True)


def package_info(text):
    fields = {}
    for line in text.splitlines():
        if line.strip() and not line.startswith("#"):
            key, value = [part.strip() for part in line.split("=", 1)]
            fields.setdefault(key, []).append(value)
    return fields


class Preparer:
    def __init__(self, workspace):
        if os.geteuid() != 0:
            raise PreparationError("preparation requires root; AUR compilation uses an unprivileged user")
        self.builder = pwd.getpwnam("rstar-build")
        if self.builder.pw_uid == 0:
            raise PreparationError("AUR builder must not be root")
        self.root = Path(os.environ["RSTAR_PLUGIN_ROOT"])
        self.cache, self.repo, self.sources = [self.root / "state" / name for name in ("packages", "repository", "sources")]
        for path in (self.cache, self.repo, self.sources):
            if not path.is_dir():
                raise PreparationError(f"missing persistent mount: {path}")
        self.arch = os.environ.get("RSTAR_DISTRIBUTION_ARCHITECTURE", os.environ.get("RSTAR_ARCHITECTURE", os.uname().machine))
        self.work = Path(workspace)
        self.work.chmod(0o755)
        self.db = self.work / "db"
        (self.db / "local").mkdir(parents=True)
        self.actual_db = Path("/var/lib/pacman")
        configuration_directory = self.work / "configuration"
        run(["bash", "-c", 'source "$1"; write_pacman_configuration "$2"',
             "rstarsleep-pacman-configuration", self.root / "lib/common.sh", configuration_directory], capture=True)
        # Resolve official targets independently of both installed and old AUR packages.
        lines, skip = [], False
        for line in (configuration_directory / "pacman.conf").read_text().splitlines():
            section = re.match(r"^\s*\[([^]]+)\]", line)
            if section:
                skip = section[1] == REPO
            if not skip:
                lines.append(line)
        self.configuration = "\n".join(lines) + "\n"
        self.official_conf, self.build_conf = self.work / "official.conf", self.work / "build.conf"
        self.official_conf.write_text(self.configuration)
        self.checkouts, self.distfiles, self.keys = [self.sources / n for n in ("aur", "distfiles", "gnupg")]
        self.outputs, self.builds = self.work / "packages", self.work / "build"
        for path in (self.checkouts, self.distfiles, self.keys, self.outputs, self.builds):
            path.mkdir(parents=True, exist_ok=True)
            os.chown(path, self.builder.pw_uid, self.builder.pw_gid)
            for parent, dirs, files in os.walk(path, followlinks=False):
                for entry in dirs + files:
                    os.chown(Path(parent) / entry, self.builder.pw_uid, self.builder.pw_gid, follow_symlinks=False)
        self.keys.chmod(0o700)
        self.makepkg_conf = self.work / "makepkg.conf"
        text = Path("/etc/makepkg.conf").read_text() + "\n"
        # --config points to our scratch copy, so makepkg would otherwise miss
        # /etc/makepkg.conf.d. Preserve the configured build settings explicitly.
        text += "\n".join(path.read_text() for path in sorted(Path("/etc/makepkg.conf.d").glob("*.conf"))) + "\n"
        self.config_sha256 = self.configuration_fingerprint(text)
        for key, path in (("SRCDEST", self.distfiles), ("BUILDDIR", self.builds)):
            text += f"{key}={shlex.quote(str(path))}\n"
        self.makepkg_conf.write_text(text)
        self.makepkg_conf.chmod(0o644)
        self.bases, self.states, self.artifacts = {}, {}, {}
        self.stack, self.files, self.resolutions = [], set(), {}
        self.official_metadata = {}
        self.planned_transactions = {}
        self.aur_devel, self.aur_rebuild = self.cache_options()
        self.cache_records = None
        self.cache_counts = dict(reused=0, built=0)
        self.refresh_repo()

    def user(self, argv, capture=False):
        return run(["runuser", "-u", "rstar-build", "--", "env",
                    f"HOME={self.builder.pw_dir}", f"GNUPGHOME={self.keys}", *argv], capture=capture)

    def makepkg(self, checkout, argv, capture=False):
        return self.user(["env", "-u", "PKGDEST", "-u", "SRCDEST", "-u", "BUILDDIR", "-u", "CARCH",
                          "bash", "-c", 'cd -- "$1"; shift; exec makepkg "$@"',
                          "rstarsleep-makepkg", str(checkout), "--config", str(self.makepkg_conf), *argv], capture=capture)

    def pacman(self, argv, isolated=False, capture=False, allowed=(0,)):
        return run(["pacman", "--config", self.official_conf if isolated else self.build_conf,
                    "--dbpath", self.db if isolated else self.actual_db,
                    "--cachedir", self.cache, "--logfile", "/dev/null", *argv], capture=capture, allowed=allowed)

    def refresh_repo(self):
        config = self.configuration
        database = self.repo / f"{REPO}.db.tar.gz"
        if database.is_file():
            config += f"\n[{REPO}]\nSigLevel = Optional TrustAll\nServer = {self.repo.as_uri()}\n"
            sync = self.actual_db / "sync"
            sync.mkdir(parents=True, exist_ok=True)
            if database.is_symlink():
                raise PreparationError("repository archive must not be a symlink")
            atomic_copy(database, sync / f"{REPO}.db")
        self.build_conf.write_text(config)

    def initialize(self, planning=False):
        log("复用独立环境已更新的仓库数据库", "Reuse the updated resolver repository databases")
        if not planning and not getattr(self, "aur_devel", True):
            log("Git 开发源码更新检查已关闭，仍检查配方变化", "Git development source checks disabled; recipe changes are still checked")
        sync = self.actual_db / "sync"
        isolated_sync = self.db / "sync"
        isolated_sync.mkdir(parents=True, exist_ok=True)
        for database in sync.glob("*"):
            if database.is_file() and database.name not in (f"{REPO}.db", f"{REPO}.db.sig"):
                atomic_copy(database, isolated_sync / database.name)
        if not list(isolated_sync.glob("*.db")):
            raise PreparationError("resolver environment has no synchronized official repository databases")
        for database in (self.db / "sync").glob("*.db"):
            with tarfile.open(database) as archive:
                for member in archive:
                    if not member.isfile() or not member.name.endswith("/desc"):
                        continue
                    fields, current = {}, None
                    for line in archive.extractfile(member).read().decode().splitlines():
                        if line.startswith("%") and line.endswith("%"):
                            current = line.strip("%")
                            fields[current] = []
                        elif line and current:
                            fields[current].append(line)
                    if fields.get("FILENAME"):
                        fields["RSTAR_REPOSITORY"] = [database.name.removesuffix(".db")]
                        self.official_metadata.setdefault(fields["FILENAME"][0], fields)
        self.refresh_repo()
        if planning:
            return
        self.baseline = set(self.pacman(["-Qq"], capture=True).stdout.splitlines())
        log(f"预下载工作环境已就绪，基线含 {len(self.baseline)} 个软件包",
            f"Predownload environment ready; baseline contains {len(self.baseline)} packages")

    def official(self, requirement):
        source, _ = target_parts(requirement)
        if source == "aur":
            return None
        if requirement in self.resolutions:
            return self.resolutions[requirement]
        result = self.pacman(["-Sp", "--noconfirm", "--print-format", "%f", "--", requirement],
                             isolated=True, capture=True, allowed=(0, 1))
        if result.returncode:
            if "target not found:" not in (result.stderr or "") and "could not find" not in (result.stderr or ""):
                raise PreparationError(f"official resolution failed: {requirement}\n{result.stderr}")
            if source is not None:
                raise PreparationError(f"target unavailable in selected repository: {requirement}")
            self.resolutions[requirement] = None
            return None
        filenames = result.stdout.splitlines()
        if not filenames or any(Path(name).name != name or name in (".", "..") for name in filenames):
            raise PreparationError(f"invalid official transaction: {requirement}")
        self.resolutions[requirement] = filenames
        return filenames

    def official_target(self, requirement):
        """Classify targets from loaded metadata without resolving each closure."""
        source, _ = target_parts(requirement)
        if source is not None:
            return self.official(requirement) is not None
        name = spec(requirement)[0]
        return any(name in fields.get("NAME", []) or name in fields.get("GROUPS", [])
                   or any(spec(value)[0] == name for value in fields.get("PROVIDES", []))
                   for fields in self.official_metadata.values())

    def collect(self, requested, prepare=False):
        """Collect only target roots and runtime dependencies, without downloading or building."""
        requested = list(dict.fromkeys(requested))
        sources = {}
        for value in requested:
            source, name = target_parts(value)
            if source is not None:
                if name in sources and sources[name] != source:
                    raise PreparationError(f"conflicting sources for package: {name}")
                sources[name] = source
        selected, visited, pending = {}, set(), []

        def select(requirement):
            official = self.official_target(requirement) if prepare else self.official(requirement) is not None
            if official:
                return
            base = self.aur(requirement)
            candidates = [name for name in base["packages"] if self.artifact_satisfies(
                dict(name=name, version=base["version"], provides=base["provides"][name]), requirement)]
            exact = [name for name in candidates if name == spec(requirement)[0]]
            candidates = exact or candidates
            if len(candidates) != 1:
                raise PreparationError(f"AUR recipe does not uniquely satisfy {requirement}")
            name = candidates[0]
            selected[name] = dict(name=name, version=base["version"], provides=base["provides"][name],
                                  dependencies=base["runtime_by_package"][name], conflicts=base["conflicts"][name])
            if name not in visited:
                visited.add(name)
                pending.extend(base["runtime_by_package"][name])

        for requirement in requested:
            spec(requirement)
            select(requirement)
        while pending:
            requirement = pending.pop()
            if any(self.artifact_satisfies(artifact, requirement) for artifact in selected.values()):
                continue
            select(requirement)
        if not requested:
            return []
        # Resolve the complete target transaction together. Independent closures
        # can contain a default provider that an explicit target later replaces.
        filenames, metadata = self._plan_transaction(requested, selected)
        if prepare:
            self.planned_transactions[tuple(requested)] = (filenames, selected)
        packages = set()
        for filename in filenames:
            names = metadata.get(filename, {}).get("NAME", [])
            if len(names) != 1 or not NAME.fullmatch(names[0]):
                raise PreparationError(f"missing planned package identity: {filename}")
            packages.add(names[0])
        return sorted(packages)

    def _plan_transaction(self, requested, selected):
        metadata = dict(self.official_metadata)

        aur_repository = None

        def resolve():
            targets = [f"{aur_repository}/{target_parts(value)[1]}" if value.startswith("aur/") else value
                       for value in requested]
            result = self.pacman(["-Sp", "--noconfirm", "--print-format", "%f", "--", *targets],
                                 isolated=True, capture=True, allowed=(0, 1))
            if result.returncode:
                raise PreparationError(f"target package plan cannot be resolved:\n{result.stderr or ''}")
            filenames = result.stdout.splitlines()
            if not filenames or any(Path(name).name != name or name in (".", "..") for name in filenames):
                raise PreparationError("invalid target package plan")
            return filenames, metadata

        if not selected:
            return resolve()
        # Pacman can account for AUR providers before any binaries exist by
        # reading a temporary sync database containing only runtime metadata.
        with tempfile.TemporaryDirectory(prefix="rstarsleep-plan-", dir=self.work) as temporary:
            repository = Path(temporary)
            database = self.db / "sync" / f"{repository.name}.db"
            configuration = repository / "pacman.conf"
            configuration.write_text(self.configuration + f"\n[{repository.name}]\nSigLevel = Never\nServer = {repository.as_uri()}\n")
            aur_repository = repository.name
            original_configuration = self.official_conf
            try:
                with tarfile.open(database, "w:gz") as archive:
                    for name, artifact in sorted(selected.items()):
                        filename = f"{name}.rstarsleep-plan.pkg.tar.zst"
                        fields = dict(NAME=[name], VERSION=[artifact["version"]], FILENAME=[filename],
                                      CSIZE=["0"], ISIZE=["0"], DEPENDS=artifact["dependencies"],
                                      PROVIDES=artifact["provides"], CONFLICTS=artifact["conflicts"])
                        metadata[filename] = fields
                        data = "".join(f"%{key}%\n" + "\n".join(values) + "\n\n" for key, values in fields.items() if values).encode()
                        member = tarfile.TarInfo(f"{name}-{artifact['version']}/desc")
                        member.size = len(data)
                        archive.addfile(member, BytesIO(data))
                self.official_conf = configuration
                return resolve()
            finally:
                self.official_conf = original_configuration
                database.unlink(missing_ok=True)

    def fetch(self, filenames):
        filenames = set(filenames)
        pending = filenames - self.files
        batches = []
        for filename in sorted(pending):
            metadata = self.official_metadata.get(filename, {})
            try:
                name, = metadata["NAME"]
                version, = metadata["VERSION"]
            except (KeyError, ValueError):
                raise PreparationError(f"missing official package identity: {filename}") from None
            spec(f"{name}={version}")
            # Pacman skips duplicate package names, even with different versions.
            for batch in batches:
                if name not in batch:
                    break
            else:
                batch = {}
                batches.append(batch)
            source = metadata.get("RSTAR_REPOSITORY", [None])[0]
            batch[name] = f"{source}/{name}={version}" if source else f"{name}={version}"
        if batches:
            log(f"下载并校验官方软件包：{len(pending)} 个文件，{len(batches)} 批",
                f"Download and verify official packages (files: {len(pending)}, batches: {len(batches)})")
        for index, batch in enumerate(batches, 1):
            if len(batches) > 1:
                log(f"处理官方软件包批次 {index}/{len(batches)}，共 {len(batch)} 个包",
                    f"Process official package batch {index}/{len(batches)} (packages: {len(batch)})")
            # -Sp already resolved each full closure. Download those exact packages
            # without choosing different providers or merging version constraints.
            # Two --nodeps flags skip re-resolution, retaining integrity/signature checks.
            self.pacman(["-Sw", "--nodeps", "--nodeps", "--noconfirm", "--", *batch.values()], isolated=True)
        for name in filenames:
            if not (self.cache / name).is_file():
                raise PreparationError(f"missing downloaded package: {name}")
        self.files.update(filenames)
        if batches:
            log(f"本阶段官方包准备完成：{len(pending)} 个新校验文件",
                f"Official packages ready for this phase: {len(pending)} newly verified files")

    def prefetch(self, requirements):
        requirements = list(dict.fromkeys(requirements))
        resolving = any(requirement not in self.resolutions for requirement in requirements)
        if resolving:
            log(f"解析 {len(requirements)} 项软件需求及完整依赖",
                f"Resolve package requirements and their full dependencies (requirements: {len(requirements)})")
        filenames, aur = set(), []
        for index, requirement in enumerate(requirements, 1):
            spec(requirement)
            closure = self.official(requirement)
            if closure is None:
                aur.append(requirement)
            else:
                filenames.update(closure)
            if resolving and (index % 25 == 0 or index == len(requirements)):
                log(f"软件需求解析进度：{index}/{len(requirements)}",
                    f"Package resolution progress: {index}/{len(requirements)}")
        self.fetch(filenames)
        return aur

    def rpc(self, method, name):
        query = {"v": "5", "type": method}
        query.update({"arg[]": name} if method == "info" else {"by": "provides", "arg": name})
        request = urllib.request.Request("https://aur.archlinux.org/rpc/?" + urllib.parse.urlencode(query), headers={"User-Agent": "RStarSleep"})
        with urllib.request.urlopen(request, timeout=60) as response:
            data = json.load(response)
        if "error" in data or not isinstance(data.get("results"), list):
            raise PreparationError(f"AUR RPC failed for {name}: {data}")
        return data["results"]

    def load_base(self, name):
        if name in self.bases:
            return self.bases[name]
        if not NAME.fullmatch(name):
            raise PreparationError(f"invalid AUR pkgbase: {name}")
        log(f"获取并解析 AUR 构建配方：{name}", f"Fetch and inspect the AUR build recipe: {name}")
        checkout = self.checkouts / name
        if checkout.exists():
            if not (checkout / ".git").is_dir():
                raise PreparationError(f"not an AUR checkout: {checkout}")
            self.user(["git", "-C", str(checkout), "fetch", "--force", "origin", "HEAD"])
            self.user(["git", "-C", str(checkout), "reset", "--hard", "FETCH_HEAD"])
            self.user(["git", "-C", str(checkout), "clean", "-fdx"])
        else:
            self.user(["git", "clone", "--", f"https://aur.archlinux.org/{name}.git", str(checkout)])
        commit = self.user(["git", "-C", str(checkout), "rev-parse", "HEAD"], capture=True).stdout.strip()
        base = srcinfo(self.makepkg(checkout, ["--printsrcinfo"], capture=True).stdout, self.arch)
        if base["name"] != name:
            raise PreparationError("AUR pkgbase disagrees with recipe")
        base.update(checkout=checkout, commit=commit)
        self.bases[name] = base
        return base

    def aur(self, requirement):
        name = spec(requirement)[0]
        info = self.rpc("info", name)
        if info:
            return self.load_base(info[0]["PackageBase"])
        providers = {base["name"] for base in self.bases.values()
                     if any(spec(p)[0] == name for values in base["provides"].values() for p in values)}
        if not providers:
            providers = {item["PackageBase"] for item in self.rpc("search", name)}
        providers = {candidate for candidate in providers
                     if any(spec(p)[0] == name for values in self.load_base(candidate)["provides"].values() for p in values)}
        if len(providers) != 1:
            raise PreparationError(f"missing or ambiguous AUR provider for {requirement}: {sorted(providers)}")
        return self.load_base(providers.pop())

    def artifact_satisfies(self, artifact, requirement):
        name, operator, expected = spec(requirement)
        if artifact["name"] == name:
            return satisfies_version(artifact["version"], operator, expected)
        for provide in artifact["provides"]:
            p_name, p_operator, p_version = spec(provide)
            if p_name == name and (operator is None or (p_operator == "=" and satisfies_version(p_version, operator, expected))):
                return True
        return False

    def ensure(self, requirement, install=False):
        spec(requirement)
        filenames = self.official(requirement)
        if filenames is not None:
            self.fetch(filenames)
            if install:
                log(f"安装官方构建依赖：{requirement}", f"Install official build dependency: {requirement}")
                self.pacman(["-S", "--needed", "--noconfirm", "--", requirement])
        else:
            base = self.aur(requirement)
            self.build(base)
            exact = [p for p in self.artifacts[base["name"]] if p["name"] == spec(requirement)[0] and self.artifact_satisfies(p, requirement)]
            providers = exact or [p for p in self.artifacts[base["name"]] if self.artifact_satisfies(p, requirement)]
            if len(providers) != 1:
                raise PreparationError(f"AUR binaries do not uniquely satisfy {requirement}")
            if install:
                log(f"安装 AUR 构建依赖：{requirement}", f"Install AUR build dependency: {requirement}")
                self.pacman(["-U", "--needed", "--noconfirm", "--", self.repo / providers[0]["filename"]])
        if install and self.pacman(["-T", target_parts(requirement)[1]], capture=True, allowed=(0, 127)).returncode:
            raise PreparationError(f"unsatisfied build dependency: {requirement}")

    @staticmethod
    def cache_options():
        def option(name, default):
            return os.environ.get("RSTAR_DISTRIBUTION_" + name, os.environ.get("RSTAR_" + name, default)).strip()
        devel = option("AURDEVEL", "true").lower()
        if devel not in ("true", "false"):
            raise PreparationError("AurDevel must be true or false")
        rebuild = set(option("AURREBUILD", "none").split())
        if (any(not NAME.fullmatch(name) for name in rebuild)
                or (rebuild & {"none", "all"} and len(rebuild) != 1)):
            raise PreparationError("AurRebuild must be none, all, or pkgbase names")
        return devel == "true", rebuild - {"none"}

    @staticmethod
    def configuration_fingerprint(configuration):
        # Exclude per-call SRCDEST/BUILDDIR/PKGDEST and unrelated mirror/target options.
        inputs = {"semantics": CACHE_FORMAT, "makepkg.conf": configuration,
                  "environment": {key: os.environ[key] for key in
                                  ("CFLAGS", "CXXFLAGS", "CPPFLAGS", "LDFLAGS", "MAKEFLAGS", "RUSTFLAGS", "SOURCE_DATE_EPOCH", "PACKAGER", "PKGEXT", "GPGKEY")
                                  if key in os.environ}}
        inputs["dropins"] = {str(path): path.read_text() for path in sorted(Path("/etc/makepkg.conf.d").glob("*.conf"))}
        return hashlib.sha256(json.dumps(inputs, sort_keys=True).encode()).hexdigest()

    def load_cache(self):
        if getattr(self, "cache_records", None) is not None:
            return self.cache_records
        path = self.repo / ".aur-builds.json"
        self.cache_index = dict(format=CACHE_FORMAT, architectures={})
        if path.is_symlink():
            raise PreparationError("AUR build index must not be a symlink")
        if path.exists():
            try:
                index = json.loads(path.read_text())
                if (not isinstance(index, dict) or index.get("format") != CACHE_FORMAT
                        or not isinstance(index.get("architectures"), dict)
                        or any(not isinstance(entries, dict) for entries in index["architectures"].values())):
                    raise ValueError("unsupported build index")
                self.cache_index = index
            except (ValueError, OSError) as error:
                log(f"AUR 构建记录无效，按需重建：{error}", f"Invalid AUR build index; rebuild on demand: {error}")
        self.cache_records = self.cache_index["architectures"].setdefault(self.arch, {})
        return self.cache_records

    def source_state(self, base):
        specs, reason = aur_cache.source_specs(base.get("sources", []), base.get("unchecked_sources", []))
        if reason:
            return specs, None, reason
        if not self.aur_devel:
            return specs, None, None
        try:
            revisions = aur_cache.remote_revisions(self.user, specs)
        except ValueError as error:
            raise PreparationError(str(error)) from error
        return specs, revisions, None

    def inspect_artifact(self, package, base):
        fields = package_info(run(["bsdtar", "-xOf", package, ".PKGINFO"], capture=True).stdout)
        if (any(len(fields.get(key, [])) != 1 for key in ("pkgname", "pkgver", "arch"))
                or not NAME.fullmatch(fields["pkgname"][0])
                or fields["arch"][0] not in ("any", self.arch)):
            raise PreparationError("invalid binary package metadata or architecture")
        artifact = dict(name=fields["pkgname"][0], version=fields["pkgver"][0],
                        architecture=fields["arch"][0], provides=fields.get("provides", []),
                        dependencies=fields.get("depend", []), conflicts=fields.get("conflict", []),
                        filename=package.name, pkgbase=base["name"], commit=base["commit"],
                        sha256=self.file_digest(package))
        for value in artifact["dependencies"] + artifact["provides"] + artifact["conflicts"]:
            spec(value)
        return artifact

    @staticmethod
    def file_digest(path):
        digest = hashlib.sha256()
        with path.open("rb") as stream:
            for chunk in iter(lambda: stream.read(1024 * 1024), b""):
                digest.update(chunk)
        return digest.hexdigest()

    def cached_artifacts(self, base, revisions, visiting=()):
        name = base["name"]
        if name in visiting:
            return None, "dependency cache cycle"
        entry = self.load_cache().get(name)
        if not isinstance(entry, dict):
            return None, "no build record"
        if entry.get("commit") != base["commit"]:
            return None, "recipe changed"
        if entry.get("configuration") != self.config_sha256:
            return None, "build configuration changed"
        if (not isinstance(entry.get("sources"), dict)
                or any(not isinstance(source, str) or not isinstance(revision, str)
                       or not aur_cache.REVISION.fullmatch(revision) for source, revision in entry["sources"].items())):
            return None, "invalid source records"
        if revisions is not None and entry.get("sources") != revisions:
            return None, "Git source changed"
        artifacts = entry.get("artifacts")
        if (not isinstance(artifacts, list) or not artifacts
                or any(not isinstance(p, dict) for p in artifacts)):
            return None, "invalid artifact inventory"
        # Never derive a path from unvalidated persistent metadata.
        for artifact in artifacts:
            filename = artifact.get("filename")
            if (not isinstance(filename, str) or Path(filename).name != filename or filename in (".", "..")
                    or not isinstance(artifact.get("sha256"), str)
                    or not re.fullmatch(r"[0-9a-f]{64}", artifact["sha256"])):
                return None, "invalid artifact identity"
            copies = [directory / filename for directory in (self.repo, self.cache)]
            good = next((path for path in copies if path.is_file() and not path.is_symlink()
                         and self.file_digest(path) == artifact["sha256"]), None)
            if good is None:
                return None, "artifact missing or checksum changed"
            try:
                if self.inspect_artifact(good, base) != artifact:
                    return None, "binary metadata changed"
            except (PreparationError, OSError, ValueError):
                return None, "invalid cached binary metadata"
            for path in copies:
                if path != good and (path.is_symlink() or not path.is_file() or self.file_digest(path) != artifact["sha256"]):
                    atomic_copy(good, path)
        names = [p["name"] for p in artifacts]
        if len(set(names)) != len(names) or set(base["packages"]) - set(names):
            return None, "incomplete split-package outputs"
        dependencies = entry.get("aur_dependencies")
        if not isinstance(dependencies, list):
            return None, "invalid dependency records"
        dependency_artifacts = []
        for dependency in dependencies:
            if (not isinstance(dependency, dict) or not isinstance(dependency.get("requirement"), str)
                    or not isinstance(dependency.get("pkgbase"), str)):
                return None, "invalid dependency identity"
            requirement = dependency["requirement"]
            if not SPEC.fullmatch(requirement) or not NAME.fullmatch(dependency["pkgbase"]):
                return None, "invalid dependency identity"
            if self.official(requirement) is not None:
                return None, "AUR build dependency provider changed"
            recipe = self.aur(requirement)
            if recipe["name"] != dependency["pkgbase"]:
                return None, "AUR build dependency provider changed"
            _, source_revisions, reason = self.source_state(recipe)
            cached, _ = (None, reason) if reason else self.cached_artifacts(recipe, source_revisions, (*visiting, name))
            if (cached is None or [{key: p[key] for key in ("name", "sha256")} for p in cached] != dependency.get("artifacts")
                    or recipe["name"] in self.aur_rebuild or "all" in self.aur_rebuild):
                return None, "AUR build dependency changed"
            dependency_artifacts.extend(cached)
        # Versioned SONAME requirements may stop resolving after an official ABI transition.
        for artifact in artifacts:
            for requirement in artifact["dependencies"]:
                depname, operator, _ = spec(requirement)
                if ".so" in depname and operator and self.official(requirement) is None:
                    candidates = artifacts + dependency_artifacts + [p for values in self.artifacts.values() for p in values]
                    if not any(self.artifact_satisfies(p, requirement) for p in candidates):
                        return None, "shared-library dependency changed"
        return artifacts, None

    def remember_build(self, base, artifacts, revisions, dependencies):
        references = []
        for requirement in dependencies:
            if self.official(requirement) is not None:
                continue
            recipe = self.aur(requirement)
            entry = self.load_cache().get(recipe["name"])
            if not isinstance(entry, dict):
                # Unverifiable dependencies also prevent safe parent reuse.
                self.load_cache().pop(base["name"], None)
                atomic_json(self.repo / ".aur-builds.json", self.cache_index)
                return
            references.append(dict(requirement=requirement, pkgbase=recipe["name"],
                                   artifacts=[{key: p[key] for key in ("name", "sha256")} for p in entry["artifacts"]]))
        self.load_cache()[base["name"]] = dict(commit=base["commit"], configuration=self.config_sha256,
                                              sources=revisions, artifacts=artifacts, aur_dependencies=references)
        atomic_json(self.repo / ".aur-builds.json", self.cache_index)

    def complete_artifacts(self, base, artifacts):
        """Rebuilt and reused packages use the same actual binary runtime metadata."""
        name = base["name"]
        self.artifacts[name] = artifacts
        base["version"] = artifacts[0]["version"]
        for artifact in artifacts:
            base["runtime_by_package"][artifact["name"]] = artifact["dependencies"]
            base["provides"][artifact["name"]] = artifact["provides"]
            base["conflicts"][artifact["name"]] = artifact["conflicts"]
        self.publish(base, artifacts)
        self.states[name] = "built"
        dependencies = list(dict.fromkeys(dependency for artifact in artifacts for dependency in artifact["dependencies"]
                                          if not any(self.artifact_satisfies(p, dependency) for p in artifacts)))
        log(f"准备 {name} 的二进制运行依赖：{len(dependencies)} 项",
            f"Prepare binary runtime dependencies: {name} ({len(dependencies)} requirements)")
        self.prefetch(dependencies)
        for dependency in dependencies:
            self.ensure(dependency)
        self.states[name] = "complete"
        self.stack.pop()

    def build(self, base):
        name = base["name"]
        if self.states.get(name) in ("built", "complete"):
            return
        if self.states.get(name) == "building":
            raise PreparationError("AUR dependency cycle: " + " -> ".join(self.stack + [name]))
        specs, revisions, reason = self.source_state(base)
        if "all" in self.aur_rebuild or name in self.aur_rebuild:
            artifacts, reason = None, "forced rebuild"
        elif reason:
            artifacts = None
        else:
            artifacts, reason = self.cached_artifacts(base, revisions)
        self.states[name] = "building"
        self.stack.append(name)
        if artifacts is not None:
            log(f"复用 AUR 构建产物：{name}", f"Reuse AUR build artifacts: {name}")
            self.complete_artifacts(base, artifacts)
            self.cache_counts["reused"] += 1
            return
        log(f"重建 AUR 软件包：{name}（{reason}）", f"Rebuild AUR package: {name} ({reason})")
        provided = set(base["packages"]) | {spec(p)[0] for values in base["provides"].values() for p in values}
        dependencies = list(dict.fromkeys(base["build"] + [dependency for dependency in base["build_runtime"]
                                                            if spec(dependency)[0] not in provided]))
        log(f"准备 {name} 的构建依赖：{len(dependencies)} 项",
                f"Prepare build dependencies for {name} (requirements: {len(dependencies)})")
        self.prefetch(dependencies)
        for dependency in dependencies:
            self.ensure(dependency, install=True)
        for fingerprint in base["keys"]:
            if not re.fullmatch(r"[0-9a-fA-F]{40}|[0-9a-fA-F]{64}", fingerprint):
                raise PreparationError("invalid source signing fingerprint")
            result = subprocess.run(["runuser", "-u", "rstar-build", "--", "env", f"GNUPGHOME={self.keys}", "gpg", "--batch", "--list-keys", fingerprint], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            if result.returncode:
                log(f"获取源码签名公钥：{fingerprint}", f"Fetch source signing public key: {fingerprint}")
                self.user(["gpg", "--batch", "--keyserver", os.environ.get("RSTAR_AURKEYSERVER", "hkps://keyserver.ubuntu.com"), "--recv-keys", fingerprint])
        # A new output directory avoids stale binaries and supports dynamic VCS versions.
        output = self.outputs / name
        output.mkdir()
        os.chown(output, self.builder.pw_uid, self.builder.pw_gid)
        self.makepkg_conf.write_text(self.makepkg_conf.read_text() + f"\nPKGDEST={shlex.quote(str(output))}\n")
        log(f"下载并校验 {name} 的源码", f"Download and verify sources for {name}")
        self.makepkg(base["checkout"], ["--verifysource", "--noconfirm"])
        cacheable = aur_cache.source_specs(base.get("sources", []), base.get("unchecked_sources", []))[1] is None
        actual = {}
        if cacheable and specs:
            try:
                if self.aur_devel:
                    # makepkg can warn and retain an old mirror on fetch failure.
                    revisions = aur_cache.remote_revisions(self.user, specs)
                actual = aur_cache.actual_revisions(self.user, specs, self.distfiles, base["checkout"])
                if self.aur_devel and actual != revisions:
                    raise PreparationError(f"downloaded Git source is stale or changed during fetch: {name}; retry preparation")
            except ValueError as error:
                raise PreparationError(str(error)) from error
        log(f"编译 AUR 软件包：{name}", f"Compile AUR package: {name}")
        # prepare() may create commits or remove .git. Record the downloaded
        # source identity above and prevent a second fetch during compilation.
        self.makepkg(base["checkout"], ["--force", "--cleanbuild", "--clean", "--holdver", "--nodeps", "--noconfirm"])
        log(f"收集并检查 {name} 的构建产物", f"Collect and inspect build artifacts for {name}")
        artifacts = []
        for package in sorted(output.glob("*.pkg.tar.*")):
            if package.name.endswith(".sig"):
                continue
            if not package.is_file() or package.is_symlink():
                raise PreparationError(f"missing or unexpected build artifact: {package}")
            artifact = self.inspect_artifact(package, base)
            for destination in (self.cache, self.repo):
                atomic_copy(package, destination / package.name)
            artifacts.append(artifact)
        if not artifacts or set(base["packages"]) - {p["name"] for p in artifacts}:
            raise PreparationError(f"incomplete split-package outputs: {name}")
        self.complete_artifacts(base, artifacts)
        if cacheable:
            self.remember_build(base, artifacts, actual, dependencies)
        else:
            self.load_cache().pop(name, None)
            atomic_json(self.repo / ".aur-builds.json", self.cache_index)
        self.cache_counts["built"] += 1
        log(f"AUR 软件包准备完成：{name}，共 {len(artifacts)} 个产物",
            f"AUR package ready: {name}, {len(artifacts)} artifacts")

    def publish(self, base, artifacts):
        log(f"发布 {base['name']} 到共享 AUR 本地仓库", f"Publish {base['name']} to the shared local AUR repository")
        database = self.repo / f"{REPO}.db.tar.gz"
        ownership_path = self.repo / ".packages.json"
        ownership = json.loads(ownership_path.read_text()) if ownership_path.exists() else {}
        if not isinstance(ownership, dict):
            raise PreparationError("invalid local repository inventory")
        for package, metadata in ownership.items():
            if not NAME.fullmatch(package) or not isinstance(metadata, dict) or not NAME.fullmatch(metadata.get("pkgbase", "")):
                raise PreparationError("invalid local repository inventory entry")
        if database.is_symlink():
            raise PreparationError("repository archive must not be a symlink")
        names = {p["name"] for p in artifacts}
        stale = [name for name, value in ownership.items() if value["pkgbase"] == base["name"] and name not in names]
        if stale and database.exists():
            run(["repo-remove", database, *stale])
        for name in stale:
            del ownership[name]
        run(["repo-add", database, *[self.repo / p["filename"] for p in artifacts]])
        ownership.update({p["name"]: p for p in artifacts})
        atomic_json(ownership_path, ownership)
        self.refresh_repo()

    def clean_dependencies(self):
        added = sorted(set(self.pacman(["-Qq"], capture=True).stdout.splitlines()) - self.baseline)
        if added:
            log(f"清理临时 AUR 构建依赖：{len(added)} 个包", f"Remove temporary AUR build dependencies: {len(added)} packages")
            self.pacman(["-R", "--noconfirm", "--", *added])
        if set(self.pacman(["-Qq"], capture=True).stdout.splitlines()) != self.baseline:
            raise PreparationError("AUR dependency cleanup changed environment baseline")

    def finish(self, requested):
        counts = getattr(self, "cache_counts", dict(reused=0, built=0))
        log(f"AUR 构建汇总：复用 {counts['reused']}，重建 {counts['built']} 个 pkgbase",
            f"AUR build summary: reused {counts['reused']}, rebuilt {counts['built']} pkgbases")
        files = self.files | {p["filename"] for values in self.artifacts.values() for p in values}
        log(f"最终校验软件包摘要和官方签名：{len(files)} 个文件",
            f"Final package checksum and official signature verification (files: {len(files)})")
        hashes = {}
        for index, name in enumerate(sorted(files), 1):
            digest = hashlib.sha256()
            with (self.cache / name).open("rb") as stream:
                for chunk in iter(lambda: stream.read(1024 * 1024), b""):
                    digest.update(chunk)
            hashes[name] = digest.hexdigest()
            if name in self.files:
                metadata = self.official_metadata.get(name)
                if not metadata or metadata.get("SHA256SUM") != [hashes[name]]:
                    raise PreparationError(f"official package checksum mismatch: {name}")
                signatures = metadata.get("PGPSIG", [])
                if len(signatures) != 1:
                    raise PreparationError(f"official package signature missing: {name}")
                signature = self.work / "verify.sig"
                signature.write_bytes(base64.b64decode(signatures[0], validate=True))
                run(["pacman-key", "--verify", signature, self.cache / name])
            if index % 25 == 0 or index == len(files):
                log(f"最终校验进度：{index}/{len(files)}", f"Final verification progress: {index}/{len(files)}")
        log("保存仓库元数据和软件准备清单", "Save repository metadata and the prepared package manifest")
        self.save_sync()
        atomic_json(self.repo / ".prepared.json", dict(format=1, architecture=self.arch, requested=requested, packages=hashes, aur=self.artifacts))
        log(f"预下载及 AUR 准备完成：{len(files)} 个软件包文件", f"Predownload and AUR preparation complete: {len(files)} package files")

    def save_sync(self):
        prepared_sync = self.repo / "official-sync"
        prepared_sync.mkdir(exist_ok=True)
        for database in (self.db / "sync").iterdir():
            if database.is_file():
                atomic_copy(database, prepared_sync / database.name)
        atomic_json(self.repo / ".planned.json", dict(format=1, architecture=self.arch))


def collect_main(arguments):
    global QUERY_MODE
    previous_query_mode = QUERY_MODE
    QUERY_MODE = True
    try:
        requested = list(dict.fromkeys(arguments))
        if any(not valid_target(name) for name in requested):
            raise PreparationError("invalid requested package")
        with tempfile.TemporaryDirectory(prefix="rstarsleep-package-plan-", dir=os.environ.get("RSTAR_PLUGIN_TMP") or None) as workspace:
            preparer = Preparer(workspace)
            preparer.initialize(planning=True)
            packages = preparer.collect(requested)
            preparer.save_sync()
    finally:
        QUERY_MODE = previous_query_mode
    json.dump(dict(schema_version=1, packages=packages), sys.stdout, sort_keys=True)
    sys.stdout.write("\n")


def prepare_systems(preparer, systems):
    """Plan each installation separately, then download their shared file union."""
    plans, requests, official_files, aur_targets = {}, {}, set(), set()
    for system_id, targets in systems.items():
        requested = list(dict.fromkeys(["base", *targets]))
        requests[system_id] = requested
        try:
            log(f"解析系统 {system_id} 的 {len(requested)} 项需求及完整依赖",
                f"Resolve {len(requested)} targets and full dependencies for system {system_id}")
            plans[system_id] = dict(schema_version=1, packages=preparer.collect(requested, prepare=True))
            filenames, selected = preparer.planned_transactions[tuple(requested)]
            official_files.update(name for name in filenames if name in preparer.official_metadata)
            aur_targets.update(selected)
        except PreparationError as error:
            raise PreparationError(f"system {system_id}: {error}") from error
    preparer.fetch(official_files)
    for target in sorted(aur_targets):
        forced = any(f"aur/{target}" in values for values in requests.values())
        preparer.ensure(f"aur/{target}" if forced else target)
        preparer.clean_dependencies()
    # Only AUR transactions need a final plan after compiling: generated binary
    # dependencies and VCS versions can differ from their source recipe metadata.
    for system_id, requested in requests.items():
        _, selected = preparer.planned_transactions[tuple(requested)]
        if selected:
            try:
                plans[system_id]["packages"] = preparer.collect(requested, prepare=True)
                filenames, _ = preparer.planned_transactions[tuple(requested)]
                preparer.fetch(name for name in filenames if name in preparer.official_metadata)
            except PreparationError as error:
                raise PreparationError(f"system {system_id}: {error}") from error
    preparer.finish(sorted({target for requested in requests.values() for target in requested}))
    return dict(schema_version=1, systems=plans)


def systems_main(argument):
    global QUERY_MODE
    request = json.loads(argument)
    if (not isinstance(request, dict) or set(request) != {"schema_version", "systems"}
            or request["schema_version"] != 1 or not isinstance(request["systems"], dict)
            or not request["systems"]):
        raise PreparationError("invalid per-system preparation request")
    for system_id, targets in request["systems"].items():
        if (not NAME.fullmatch(system_id) or not isinstance(targets, list)
                or any(not isinstance(name, str) or not valid_target(name) for name in targets)):
            raise PreparationError("invalid system identity or configured package targets")
    previous_query_mode, QUERY_MODE = QUERY_MODE, True
    try:
        with tempfile.TemporaryDirectory(prefix="rstarsleep-predownload-", dir=os.environ.get("RSTAR_PLUGIN_TMP") or None) as workspace:
            preparer = Preparer(workspace)
            preparer.initialize()
            result = prepare_systems(preparer, request["systems"])
    finally:
        QUERY_MODE = previous_query_mode
    json.dump(result, sys.stdout, sort_keys=True)
    sys.stdout.write("\n")


def main():
    requested = list(dict.fromkeys(["base", *sys.argv[1:]]))
    if any(not valid_target(name) for name in requested):
        raise PreparationError("invalid requested package")
    log(f"开始共享软件准备：{len(requested)} 项需求（含 base）",
        f"Start shared package preparation: {len(requested)} requirements (including base)")
    with tempfile.TemporaryDirectory(prefix="rstarsleep-predownload-", dir=os.environ.get("RSTAR_PLUGIN_TMP") or None) as workspace:
        preparer = Preparer(workspace)
        preparer.initialize()
        aur_targets = preparer.prefetch(requested)
        if aur_targets:
            log(f"解析并构建 AUR 软件：{len(aur_targets)} 项需求", f"Resolve and build AUR packages (requirements: {len(aur_targets)})")
        for package in aur_targets:
            info = preparer.rpc("info", package)
            if info:
                preparer.load_base(info[0]["PackageBase"])
        for package in aur_targets:
            preparer.aur(package)
        for package in aur_targets:
            preparer.ensure(package)
            preparer.clean_dependencies()
        preparer.finish(requested)


if __name__ == "__main__":
    try:
        if sys.argv[1:2] == ["--systems-json"]:
            if len(sys.argv) != 3:
                raise PreparationError("expected one per-system preparation JSON argument")
            systems_main(sys.argv[2])
        elif sys.argv[1:2] == ["--collect"]:
            collect_main(sys.argv[2:])
        else:
            main()
    except (PreparationError, OSError, KeyError, ValueError, urllib.error.URLError, tarfile.TarError) as error:
        log(f"预下载失败：{error}", f"Predownload failed: {error}")
        sys.exit(1)

#!/usr/bin/env python3
"""Prepare shared official package closures and a locally built AUR repository."""
import base64
import hashlib
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
from pathlib import Path

NAME = re.compile(r"^[A-Za-z0-9@_+][A-Za-z0-9@._+-]*$")
SPEC = re.compile(r"^([A-Za-z0-9@_+][A-Za-z0-9@._+-]*)(?:(>=|<=|=|>|<)([^\s]+))?$")
REPO = "rstarsleep"


class PreparationError(Exception):
    pass


def log(message):
    print(f"Arch predownload: {message}", file=sys.stderr, flush=True)


def run(argv, capture=False, allowed=(0,)):
    result = subprocess.run(list(map(str, argv)), text=True,
                            stdout=subprocess.PIPE if capture else None,
                            stderr=subprocess.PIPE if capture else None)
    if result.returncode not in allowed:
        raise PreparationError(f"command failed ({result.returncode}): {shlex.join(list(map(str, argv)))}\n{result.stderr or ''}")
    return result


def spec(value):
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

    runtime, build, provides = [], values(base, "makedepends") + values(base, "checkdepends"), {}
    for package, fields in packages.items():
        arches = fields.get("arch", base.get("arch", []))
        if arches and "any" not in arches and architecture not in arches:
            raise PreparationError(f"{package} does not support {architecture}")
        runtime.extend(inherited(fields, "depends"))
        build.extend(v for v in values(fields, "makedepends") + values(fields, "checkdepends") if v)
        provides[package] = inherited(fields, "provides")
    for dependency in runtime + build:
        spec(dependency)
    return dict(name=name, packages=packages, runtime=list(dict.fromkeys(runtime)),
                build=list(dict.fromkeys(build)), build_runtime=[v for v in values(base, "depends") if v], provides=provides, keys=base.get("validpgpkeys", []))


def atomic_copy(source, destination):
    destination = Path(destination)
    with tempfile.NamedTemporaryFile(dir=destination.parent, delete=False) as stream:
        temporary = Path(stream.name)
        try:
            with Path(source).open("rb") as src:
                shutil.copyfileobj(src, stream)
            stream.flush()
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
        self.arch = os.environ.get("RSTAR_ARCHITECTURE", os.uname().machine)
        self.work = Path(workspace)
        self.work.chmod(0o755)
        self.db = self.work / "db"
        (self.db / "local").mkdir(parents=True)
        self.actual_db = Path("/var/lib/pacman")
        # Resolve official targets independently of both installed and old AUR packages.
        lines, skip = [], False
        for line in Path("/etc/pacman.conf").read_text().splitlines():
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
        for key, path in (("SRCDEST", self.distfiles), ("BUILDDIR", self.builds)):
            text += f"{key}={shlex.quote(str(path))}\n"
        self.makepkg_conf.write_text(text)
        self.bases, self.states, self.artifacts = {}, {}, {}
        self.stack, self.files, self.fetched = [], set(), set()
        self.official_metadata = {}
        self.refresh_repo()

    def user(self, argv, capture=False):
        return run(["runuser", "-u", "rstar-build", "--", "env",
                    f"HOME={self.builder.pw_dir}", f"GNUPGHOME={self.keys}", *argv], capture=capture)

    def makepkg(self, checkout, argv, capture=False):
        return self.user(["bash", "-c", 'cd -- "$1"; shift; exec makepkg "$@"',
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

    def initialize(self):
        self.pacman(["-Sy", "--noconfirm"], isolated=True)
        sync = self.actual_db / "sync"
        sync.mkdir(parents=True, exist_ok=True)
        for database in (self.db / "sync").glob("*"):
            if database.is_file():
                atomic_copy(database, sync / database.name)
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
                        self.official_metadata.setdefault(fields["FILENAME"][0], fields)
        self.refresh_repo()
        self.pacman(["-Su", "--noconfirm"])
        self.baseline = set(self.pacman(["-Qq"], capture=True).stdout.splitlines())

    def official(self, requirement):
        result = self.pacman(["-Sp", "--noconfirm", "--print-format", "%f", "--", requirement],
                             isolated=True, capture=True, allowed=(0, 1))
        if result.returncode:
            if "target not found:" not in (result.stderr or "") and "could not find" not in (result.stderr or ""):
                raise PreparationError(f"official resolution failed: {requirement}\n{result.stderr}")
            return None
        filenames = result.stdout.splitlines()
        if not filenames or any(Path(name).name != name or name in (".", "..") for name in filenames):
            raise PreparationError(f"invalid official transaction: {requirement}")
        return filenames

    def fetch(self, requirement, filenames):
        if requirement not in self.fetched:
            self.pacman(["-Sw", "--noconfirm", "--", requirement], isolated=True)
            self.fetched.add(requirement)
        for name in filenames:
            if not (self.cache / name).is_file():
                raise PreparationError(f"missing downloaded package: {name}")
            self.files.add(name)

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
            self.fetch(requirement, filenames)
            if install:
                self.pacman(["-S", "--needed", "--noconfirm", "--", requirement])
        else:
            base = self.aur(requirement)
            self.build(base)
            exact = [p for p in self.artifacts[base["name"]] if p["name"] == spec(requirement)[0] and self.artifact_satisfies(p, requirement)]
            providers = exact or [p for p in self.artifacts[base["name"]] if self.artifact_satisfies(p, requirement)]
            if len(providers) != 1:
                raise PreparationError(f"AUR binaries do not uniquely satisfy {requirement}")
            if install:
                self.pacman(["-U", "--needed", "--noconfirm", "--", self.repo / providers[0]["filename"]])
        if install and self.pacman(["-T", requirement], capture=True, allowed=(0, 127)).returncode:
            raise PreparationError(f"unsatisfied build dependency: {requirement}")

    def build(self, base):
        name = base["name"]
        if self.states.get(name) in ("built", "complete"):
            return
        if self.states.get(name) == "building":
            raise PreparationError("AUR dependency cycle: " + " -> ".join(self.stack + [name]))
        self.states[name] = "building"
        self.stack.append(name)
        for dependency in base["build"]:
            self.ensure(dependency, install=True)
        provided = set(base["packages"]) | {spec(p)[0] for values in base["provides"].values() for p in values}
        for dependency in base["build_runtime"]:
            if spec(dependency)[0] not in provided:
                self.ensure(dependency, install=True)
        for fingerprint in base["keys"]:
            if not re.fullmatch(r"[0-9a-fA-F]{40}|[0-9a-fA-F]{64}", fingerprint):
                raise PreparationError("invalid source signing fingerprint")
            result = subprocess.run(["runuser", "-u", "rstar-build", "--", "env", f"GNUPGHOME={self.keys}", "gpg", "--batch", "--list-keys", fingerprint], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            if result.returncode:
                self.user(["gpg", "--batch", "--keyserver", os.environ.get("RSTAR_AURKEYSERVER", "hkps://keyserver.ubuntu.com"), "--recv-keys", fingerprint])
        # A new output directory avoids stale binaries and supports dynamic VCS versions.
        output = self.outputs / name
        output.mkdir()
        os.chown(output, self.builder.pw_uid, self.builder.pw_gid)
        self.makepkg_conf.write_text(self.makepkg_conf.read_text() + f"\nPKGDEST={shlex.quote(str(output))}\n")
        self.makepkg(base["checkout"], ["--verifysource", "--noconfirm"])
        self.makepkg(base["checkout"], ["--force", "--cleanbuild", "--clean", "--nodeps", "--noconfirm"])
        artifacts = []
        for package in sorted(output.glob("*.pkg.tar.*")):
            if package.name.endswith(".sig"):
                continue
            if not package.is_file() or package.is_symlink():
                raise PreparationError(f"missing or unexpected build artifact: {package}")
            fields = package_info(run(["bsdtar", "-xOf", package, ".PKGINFO"], capture=True).stdout)
            if len(fields.get("pkgname", [])) != 1 or len(fields.get("pkgver", [])) != 1:
                raise PreparationError("invalid binary package metadata")
            artifact = dict(name=fields["pkgname"][0], version=fields["pkgver"][0],
                            provides=fields.get("provides", []), dependencies=fields.get("depend", []),
                            filename=package.name, pkgbase=name, commit=base["commit"])
            if not NAME.fullmatch(artifact["name"]):
                raise PreparationError("invalid binary package name")
            for destination in (self.cache, self.repo):
                atomic_copy(package, destination / package.name)
            artifacts.append(artifact)
        if not artifacts or set(base["packages"]) - {p["name"] for p in artifacts}:
            raise PreparationError(f"incomplete split-package outputs: {name}")
        self.artifacts[name] = artifacts
        self.publish(base, artifacts)
        self.states[name] = "built"
        for artifact in artifacts:
            for dependency in artifact["dependencies"]:
                if not any(self.artifact_satisfies(p, dependency) for p in artifacts):
                    self.ensure(dependency)
        self.states[name] = "complete"
        self.stack.pop()

    def publish(self, base, artifacts):
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
            self.pacman(["-R", "--noconfirm", "--", *added])
        if set(self.pacman(["-Qq"], capture=True).stdout.splitlines()) != self.baseline:
            raise PreparationError("AUR dependency cleanup changed environment baseline")

    def finish(self, requested):
        files = self.files | {p["filename"] for values in self.artifacts.values() for p in values}
        hashes = {}
        for name in sorted(files):
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
        prepared_sync = self.repo / "official-sync"
        prepared_sync.mkdir(exist_ok=True)
        for database in (self.db / "sync").iterdir():
            if database.is_file():
                atomic_copy(database, prepared_sync / database.name)
        atomic_json(self.repo / ".prepared.json", dict(format=1, architecture=self.arch, requested=requested, packages=hashes, aur=self.artifacts))
        log(f"prepared {len(files)} package files")


def main():
    requested = list(dict.fromkeys(["base", *sys.argv[1:]]))
    if any(not NAME.fullmatch(name) for name in requested):
        raise PreparationError("invalid requested package")
    with tempfile.TemporaryDirectory(prefix="rstarsleep-predownload-") as workspace:
        preparer = Preparer(workspace)
        preparer.initialize()
        aur_targets = [package for package in requested if preparer.official(package) is None]
        for package in aur_targets:
            info = preparer.rpc("info", package)
            if info:
                preparer.load_base(info[0]["PackageBase"])
        for package in aur_targets:
            preparer.aur(package)
        for package in requested:
            preparer.ensure(package)
            preparer.clean_dependencies()
        preparer.finish(requested)


if __name__ == "__main__":
    try:
        main()
    except (PreparationError, OSError, KeyError, ValueError, urllib.error.URLError, tarfile.TarError) as error:
        log(str(error))
        sys.exit(1)

"""Plan and maintain the cumulative AUR build environment. / 规划并维护累积式 AUR 构建环境。"""
import hashlib
import json
import re
import sys
import time


class DependencyConflict(Exception):
    """Only dependency conflicts may select the legacy workflow. / 仅依赖冲突允许选择旧流程。"""


def requirement_name(value):
    return re.split(r"[<>=]", value.split("/", 1)[-1], maxsplit=1)[0]


class BuildPlan:
    """Discover recipes and all dependencies before mutation. / 修改环境前发现全部配方和依赖。"""

    def __init__(self, preparer, targets, error=ValueError):
        self.preparer, self.error = preparer, error
        self.bases, self.dependencies, self.providers = {}, {}, {}
        self.selected, self.dependency_requirements = {}, set()
        self.official_requirements, self.official_files, self.order = [], set(), []
        self.targets = list(dict.fromkeys(targets))
        pending = list(reversed(self.targets))
        seen = set()
        while pending:
            requirement = pending.pop()
            if requirement in seen:
                continue
            seen.add(requirement)
            files = preparer.official(requirement)
            if files is not None:
                self.official_requirements.append(requirement)
                self.official_files.update(files)
                continue
            base = preparer.aur(requirement)
            self.providers[requirement] = base["name"]
            candidates = [dict(name=name, version=base['version'], provides=base['provides'][name],
                               conflicts=base['conflicts'][name], dependencies=base['runtime_by_package'][name])
                          for name in base['packages']]
            candidates = [value for value in candidates if preparer.artifact_satisfies(value, requirement)]
            exact = [value for value in candidates if value['name'] == requirement_name(requirement)]
            candidates = exact or candidates
            if len(candidates) != 1:
                raise error(f'AUR 依赖无法唯一满足：{requirement} / AUR dependency is not uniquely satisfied: {requirement}')
            self.selected[requirement] = candidates[0]
            if base["name"] in self.bases:
                continue
            self.bases[base["name"]] = base
            for fingerprint in base["keys"]:
                if not re.fullmatch(r"[0-9a-fA-F]{40}|[0-9a-fA-F]{64}", fingerprint):
                    raise error('源码签名指纹无效 / invalid source signing fingerprint')
            own = [dict(name=name, version=base["version"], provides=base["provides"][name])
                   for name in base["packages"]]
            def external(value):
                return not any(preparer.artifact_satisfies(artifact, value) for artifact in own)
            self.dependencies[base["name"]] = list(dict.fromkeys(
                value for value in base["build"] + base["build_runtime"] if external(value)))
            # Split outputs can introduce runtime-only recipes; discover them too.
            # 拆分产物可能引入仅运行时需要的配方，同样提前发现。
            all_dependencies = list(dict.fromkeys(self.dependencies[base["name"]]
                                                 + [value for value in base["runtime"] if external(value)]))
            self.dependency_requirements.update(all_dependencies)
            pending.extend(reversed(all_dependencies))
        visiting, visited = [], set()
        edges = {}
        for name, requirements in self.dependencies.items():
            edges[name], pending, seen_requirements = set(), list(requirements), set()
            while pending:
                requirement = pending.pop()
                if requirement in seen_requirements:
                    continue
                seen_requirements.add(requirement)
                if requirement in self.providers:
                    edges[name].add(self.providers[requirement])
                    # Installing a build dependency also needs its runtime closure ready.
                    # 安装构建依赖时，其运行依赖闭包也必须已经准备好。
                    pending.extend(self.selected[requirement]['dependencies'])
        def visit(name):
            if name in visiting:
                chain = ' -> '.join(visiting + [name])
                raise error(f'AUR 构建依赖循环：{chain} / AUR build dependency cycle: {chain}')
            if name in visited:
                return
            visiting.append(name)
            for dependency in sorted(edges[name]):
                visit(dependency)
            visiting.pop()
            visited.add(name)
            self.order.append(name)
        for name in sorted(self.bases):
            visit(name)


class BuildEnvironment:
    """Keep needed dependencies and install each artifact once. / 保留所需依赖，每份产物仅安装一次。"""

    def __init__(self, preparer, targets, *, error, log, write_json):
        self.p, self.error, self.log, self.write_json = preparer, error, log, write_json
        self.plan = BuildPlan(preparer, targets, error)
        self.fallback = False
        self.installed = set()
        self.receipt = preparer.actual_db.parent / 'rstarsleep/aur-installed.json'
        self.receipts = {}
        if self.receipt.is_symlink():
            raise error('AUR 安装记录不能是符号链接 / AUR installation record must not be a symlink')
        if self.receipt.exists():
            try:
                value = json.loads(self.receipt.read_text())
                if value.get('format') != 1 or not isinstance(value.get('packages'), dict):
                    raise ValueError('format')
                self.receipts = value['packages']
            except (ValueError, AttributeError) as failure:
                raise error(f'AUR 安装记录无效 / invalid AUR installation record: {failure}') from failure
        roots = ['base', 'git', 'base-devel', 'python', 'sudo', *preparer.keyring_targets]
        self.baseline_files, metadata = preparer._plan_transaction(roots, {})
        self.baseline = {metadata[value]['NAME'][0] for value in self.baseline_files}
        self.official_files = set(self.baseline_files) | self.plan.official_files
        self.keep = set(self.baseline)
        self.keep.update(preparer.official_metadata[value]['NAME'][0] for value in self.official_files)
        # Build outputs themselves need not be installed; retain only dependency outputs.
        # 构建产物自身无需安装；仅保留作为依赖使用的产物。
        self.keep.update(self.plan.selected[value]['name'] for value in self.plan.dependency_requirements
                         if value in self.plan.selected)

    def inventory(self):
        result = self.p.pacman(['-Qq'], capture=True)
        names = result.stdout.splitlines()
        if len(set(names)) != len(names) or any(not re.fullmatch(r'[A-Za-z0-9@_+][A-Za-z0-9@._+-]*', n) for n in names):
            raise self.error('构建环境软件清单无效 / invalid build environment inventory')
        return set(names)

    def prune(self, keep):
        obsolete = sorted(self.inventory() - keep)
        if obsolete:
            self.log(f'构建前清理废依赖：{len(obsolete)} 个包',
                     f'Prune obsolete dependencies before building: {len(obsolete)} packages')
            # Retained old versions may still need a package until the upgrade completes.
            # 升级完成前，保留的旧版本可能仍需要某个包。
            self.transaction(['-R', '--unneeded', '--noconfirm', '--', *obsolete])

    def transaction(self, arguments, *, official=False):
        result = self.p.pacman(arguments, official_only=official, capture=True, allowed=(0, 1))
        if result.returncode:
            diagnostic = result.stderr or ''
            # These diagnostics come from preparation, before any package is committed.
            # 这些诊断来自事务准备阶段，此时尚未提交任何软件包。
            if re.search(r'error: failed to prepare transaction \((?:conflicting dependencies|could not satisfy dependencies)\)', diagnostic):
                raise DependencyConflict(diagnostic.strip())
            raise self.error(f'构建环境维护失败 / build environment maintenance failed:\n{diagnostic}')
        for output in (result.stdout, result.stderr):
            if output:
                print(output, end='' if output.endswith('\n') else '\n', file=sys.stderr, flush=True)

    def official_targets(self, files):
        targets = []
        versions = {}
        for filename in sorted(files):
            fields = self.p.official_metadata[filename]
            name, version = fields['NAME'][0], fields['VERSION'][0]
            if name in versions and versions[name] != version:
                raise DependencyConflict(f'仓库依赖版本冲突：{name} / conflicting repository dependency versions: {name}')
            versions[name] = version
            repository = fields.get('RSTAR_REPOSITORY', [None])[0]
            targets.append(f'{repository}/{name}={version}' if repository else f'{name}={version}')
        return list(dict.fromkeys(targets))

    def check_conflicts(self):
        artifacts = []
        for filename in sorted(self.official_files):
            fields = self.p.official_metadata[filename]
            artifacts.append(dict(name=fields['NAME'][0], version=fields['VERSION'][0],
                                  provides=fields.get('PROVIDES', []), conflicts=fields.get('CONFLICTS', [])))
        # Check future AUR installations too, while all recipe metadata is available.
        # 配方元数据齐备时，同样检查后续 AUR 依赖安装的冲突。
        artifacts.extend(self.plan.selected[value] for value in sorted(self.plan.dependency_requirements)
                         if value in self.plan.selected)
        for index, artifact in enumerate(artifacts):
            for other in artifacts[index + 1:]:
                if artifact['name'] == other['name']:
                    if artifact['version'] != other['version']:
                        raise DependencyConflict(f"依赖版本冲突：{artifact['name']} / conflicting dependency versions: {artifact['name']}")
                    continue
                if any(self.p.artifact_satisfies(other, value) for value in artifact['conflicts']) or any(
                        self.p.artifact_satisfies(artifact, value) for value in other['conflicts']):
                    raise DependencyConflict(f"依赖冲突：{artifact['name']} / {other['name']} / conflicting dependencies")

    def install_official(self, files):
        targets = self.official_targets(files)
        if targets:
            self.p.fetch(files)
            self.transaction(['-S', '--needed', '--noconfirm', '--', *targets], official=True)

    def prepare(self):
        self.log(f'完整 AUR 构建依赖已解析：{len(self.plan.bases)} 个 pkgbase',
                 f'Complete AUR build dependencies resolved: {len(self.plan.bases)} pkgbases')
        try:
            self.check_conflicts()
            self.official_targets(self.official_files)
            self.prune(self.keep)
            self.install_official(self.official_files)
            self.prune(self.keep)
        except DependencyConflict as conflict:
            self.use_fallback(conflict)

    def use_fallback(self, conflict):
        if self.fallback:
            raise self.error(f'逐目标构建仍存在依赖冲突 / dependency conflict remains in per-target builds:\n{conflict}')
        self.log(f'警告：累积构建依赖冲突，三秒后回退逐目标维护：{conflict}',
                 f'Warning: cumulative build dependency conflict; fall back to per-target maintenance in 3 seconds: {conflict}')
        time.sleep(3)
        self.fallback = True
        try:
            self.prune(self.baseline)
            self.install_official(set(self.baseline_files))
            self.prune(self.baseline)
        except DependencyConflict as failure:
            raise self.error(f'基础构建环境依赖冲突 / base build environment dependency conflict:\n{failure}') from failure
        actual = self.inventory()
        if actual != self.baseline:
            raise self.error('无法恢复逐目标构建基线 / cannot restore the per-target build baseline')
        self.p.baseline = actual
        self.p.states.clear()
        self.p.stack.clear()
        self.p.artifacts.clear()
        self.installed.clear()

    def ensure_official(self, requirement):
        if not self.p.pacman(['-T', requirement.split('/', 1)[-1]], capture=True, allowed=(0, 127)).returncode:
            return
        self.install_official(set(self.p.official(requirement)))

    def install_aur(self, selected):
        pending, artifacts, visited = [selected], {}, set()
        while pending:
            artifact = pending.pop()
            if artifact['name'] in visited:
                continue
            visited.add(artifact['name'])
            identity = (artifact['name'], artifact['version'], artifact['sha256'])
            if identity not in self.installed:
                artifacts[artifact['name']] = artifact
            for requirement in artifact['dependencies']:
                official = self.p.official(requirement)
                if official is not None:
                    self.ensure_official(requirement)
                    continue
                candidates = [value for values in self.p.artifacts.values() for value in values
                              if self.p.artifact_satisfies(value, requirement)]
                exact = [value for value in candidates if value['name'] == requirement_name(requirement)]
                candidates = exact or candidates
                if len(candidates) != 1:
                    raise self.error(f'AUR 运行依赖无法唯一满足：{requirement} / AUR runtime dependency is not uniquely satisfied: {requirement}')
                pending.append(candidates[0])
        if not artifacts:
            return
        changed = []
        for name, artifact in sorted(artifacts.items()):
            database = self.database_identity(artifact)
            identity = dict(version=artifact['version'], sha256=artifact['sha256'], database=database)
            installed = self.p.pacman(['-Q', '--', name], capture=True, allowed=(0, 1))
            if database is None or installed.returncode or installed.stdout.strip() != f"{name} {artifact['version']}" or self.receipts.get(name) != identity:
                changed.append(artifact)
        if changed:
            self.log(f'安装或更新 AUR 构建依赖：{len(changed)} 个包',
                     f'Install or update AUR build dependencies: {len(changed)} packages')
            # A single transaction also supports runtime cycles among available artifacts.
            # 单个事务同样支持已生成产物之间的运行依赖环。
            self.transaction(['-U', '--noconfirm', '--', *[str(self.p.repo / a['filename']) for a in changed]])
        for artifact in artifacts.values():
            self.installed.add((artifact['name'], artifact['version'], artifact['sha256']))
            self.receipts[artifact['name']] = dict(version=artifact['version'], sha256=artifact['sha256'],
                                                    database=self.database_identity(artifact))
        self.receipt.parent.mkdir(parents=True, exist_ok=True)
        self.write_json(self.receipt, dict(format=1, packages=self.receipts))

    def database_identity(self, artifact):
        version = artifact['version']
        if not version or any(character.isspace() or character in '/\\\0' for character in version):
            raise self.error('AUR 软件包版本不能构成数据库路径 / AUR package version cannot form a database path')
        descriptor = self.p.actual_db / 'local' / f"{artifact['name']}-{artifact['version']}" / 'desc'
        if descriptor.is_symlink() or not descriptor.is_file():
            return None
        stat = descriptor.stat()
        # Metadata survives snapshots, but detects a same-version reinstall from another source.
        # 元数据随快照保留，同时检测其他来源的同版本重装。
        return dict(size=stat.st_size, mtime=stat.st_mtime_ns, ctime=stat.st_ctime_ns,
                    sha256=hashlib.sha256(descriptor.read_bytes()).hexdigest())

    def build_targets(self, targets):
        while True:
            try:
                for target in targets:
                    self.p.ensure(target)
                    self.p.clean_dependencies()
                return
            except DependencyConflict as conflict:
                self.use_fallback(conflict)

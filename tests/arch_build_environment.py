"""Cumulative dependency maintenance and conflict fallback. / 累积依赖维护及冲突回退。"""
import hashlib
import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import Mock, patch

from arch_predownload import arch

environment = arch.build_environment


class CumulativeEnvironment(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.recipes, self.commands, self.events = {}, [], []
        self.versions = {'base': '1-1', 'git': '1-1', 'base-devel': '1-1', 'python': '1-1', 'sudo': '1-1'}
        self.installed = dict(self.versions)
        self.metadata = {}
        for name in self.versions:
            self.official_package(name)
        self.p = arch.Preparer.__new__(arch.Preparer)
        p = self.p
        p.actual_db, p.repo = self.root / 'var/lib/pacman', self.root / 'repo'
        p.repo.mkdir()
        p.keyring_targets, p.official_metadata = [], self.metadata
        p.states, p.stack, p.artifacts = {}, [], {}
        p.baseline = set(self.installed)
        p.official = Mock(side_effect=self.official)
        p.aur = Mock(side_effect=lambda value: self.recipes[environment.requirement_name(value)])
        p._plan_transaction = Mock(side_effect=lambda roots, selected: (
            [self.filename(value) for value in self.versions], self.metadata))
        p.pacman = Mock(side_effect=self.pacman)
        p.fetch = Mock(side_effect=lambda files: self.events.append(('fetch', set(files))))

    def filename(self, name, version='1-1'):
        return f'{name}-{version}-x86_64.pkg.tar.zst'

    def official_package(self, name, *, depends=(), conflicts=(), provides=(), version='1-1'):
        filename = self.filename(name, version)
        self.metadata[filename] = dict(NAME=[name], VERSION=[version], DEPENDS=list(depends),
                                       PROVIDES=list(provides), CONFLICTS=list(conflicts),
                                       RSTAR_REPOSITORY=['fixture'])
        return filename

    def official(self, requirement):
        if requirement.startswith('aur/'):
            return None
        name = environment.requirement_name(requirement)
        matching = [filename for filename, fields in self.metadata.items() if fields['NAME'] == [name]]
        if not matching:
            return None
        result = set(matching)
        for filename in matching:
            for dependency in self.metadata[filename]['DEPENDS']:
                result.update(self.official(dependency))
        return sorted(result)

    def recipe(self, name, *, build=(), runtime=(), base_runtime=(), conflicts=(), extra=''):
        text = f'pkgbase = {name}\npkgver = 1\npkgrel = 1\n'
        text += ''.join(f'depends = {value}\n' for value in base_runtime)
        text += ''.join(f'makedepends = {value}\n' for value in build)
        text += f'pkgname = {name}\n'
        text += ''.join(f'depends = {value}\n' for value in runtime)
        text += ''.join(f'conflicts = {value}\n' for value in conflicts)
        result = arch.srcinfo(text + extra, 'x86_64')
        self.recipes[name] = result
        return result

    def pacman(self, args, **kwargs):
        self.commands.append(list(map(str, args)))
        operation = args[0]
        if operation == '-Qq':
            return subprocess.CompletedProcess(args, 0, '\n'.join(sorted(self.installed)) + '\n', '')
        if operation == '-Q':
            name = args[-1]
            version = self.installed.get(name)
            return subprocess.CompletedProcess(args, 0 if version else 1, f'{name} {version}\n' if version else '', '')
        if operation == '-T':
            name = environment.requirement_name(args[-1])
            provided = any(value['name'] in self.installed and self.p.artifact_satisfies(value, args[-1])
                           for values in self.p.artifacts.values() for value in values)
            return subprocess.CompletedProcess(args, 0 if name in self.installed or provided else 127, '', '')
        if operation == '-R':
            for name in args[args.index('--') + 1:]:
                self.installed.pop(name, None)
            self.events.append(('prune', set(args[args.index('--') + 1:])))
        elif operation == '-S':
            for target in args[args.index('--') + 1:]:
                name, version = target.split('/', 1)[-1].split('=', 1)
                self.installed[name] = version
            self.events.append(('official', set(self.installed)))
        elif operation == '-U':
            files = {Path(value).name for value in args[args.index('--') + 1:]}
            for values in self.p.artifacts.values():
                for artifact in values:
                    if artifact['filename'] in files:
                        self.installed[artifact['name']] = artifact['version']
                        descriptor = self.p.actual_db / 'local' / f"{artifact['name']}-{artifact['version']}" / 'desc'
                        descriptor.parent.mkdir(parents=True, exist_ok=True)
                        descriptor.write_text(artifact['sha256'])
            self.events.append(('aur', files))
        else:
            raise AssertionError(args)
        return subprocess.CompletedProcess(args, 0, '', '')

    def manager(self, targets):
        manager = environment.BuildEnvironment(self.p, targets, error=arch.PreparationError,
                                                log=Mock(), write_json=arch.atomic_json)
        self.p.build_environment = manager
        return manager

    def artifact(self, name, dependencies=(), version='1-1', content='same'):
        value = dict(name=name, version=version, sha256=hashlib.sha256(content.encode()).hexdigest(),
                     filename=self.filename(name, version), dependencies=list(dependencies), provides=[], conflicts=[])
        self.p.artifacts[name] = [value]
        return value

    def test_all_build_check_and_split_runtime_dependencies_discovered_before_installing(self):
        self.official_package('compiler', depends=['lib'])
        self.official_package('lib')
        self.recipe('consumer', build=['tool'], extra='pkgname = consumer-gui\ndepends = gui-runtime\n')
        self.recipe('tool', build=['compiler'], extra='checkdepends = checker\n')
        self.recipe('checker')
        self.recipe('gui-runtime')
        manager = self.manager(['consumer'])
        self.assertEqual(set(manager.plan.bases), {'consumer', 'tool', 'checker', 'gui-runtime'})
        self.assertLess(manager.plan.order.index('checker'), manager.plan.order.index('tool'))
        self.assertLess(manager.plan.order.index('tool'), manager.plan.order.index('consumer'))
        self.assertIn(self.filename('compiler'), manager.official_files)
        self.assertIn(self.filename('lib'), manager.official_files)
        self.assertFalse(self.commands)
        self.p.fetch.assert_not_called()

    def test_real_build_chain_keeps_shared_dependencies_and_installs_them_once(self):
        self.official_package('compiler')
        self.recipe('one', build=['tool'])
        self.recipe('two', build=['tool'])
        self.recipe('tool', build=['compiler'])
        for base in self.recipes.values():
            base.update(checkout=self.root, commit='fixture-recipe')
        p = self.p
        p.outputs, p.cache = self.root / 'output', self.root / 'cache'
        p.outputs.mkdir()
        p.cache.mkdir()
        p.makepkg_conf = self.root / 'makepkg.conf'
        p.makepkg_conf.write_text('')
        p.builder = Mock(pw_uid=os.getuid(), pw_gid=os.getgid())
        p.aur_devel, p.aur_rebuild, p.resolutions = True, False, {}
        p.cache_counts = dict(reused=0, built=0)
        p.source_state = Mock(return_value=([], None, 'fixture rebuild'))
        p.makepkg, p.publish, p.remember_build, p.save_progress = Mock(), Mock(), Mock(), Mock()
        compiled = []
        def compile_package(base, output):
            self.assertEqual(set(p.build_environment.plan.bases), {'one', 'two', 'tool'})
            self.assertIn('compiler', self.installed)
            if base['name'] != 'tool':
                self.assertIn('tool', self.installed)
            compiled.append(base['name'])
            artifact = self.artifact(base['name'])
            (output / artifact['filename']).write_bytes(b'same')
            return output
        p.compile_package = Mock(side_effect=compile_package)
        p.inspect_artifact = Mock(side_effect=lambda package, base: p.artifacts[base['name']][0])
        manager = self.manager(['one', 'two'])
        manager.prepare()
        p.prepare_targets(['one', 'two'])
        self.assertEqual(compiled, ['tool', 'one', 'two'])
        self.assertEqual(sum(call[0] == '-S' for call in self.commands), 1)
        self.assertEqual(sum(call[0] == '-U' for call in self.commands), 1)
        self.assertTrue({'compiler', 'tool'} <= set(self.installed))
        self.assertFalse(any(call[0] == '-R' for call in self.commands))

    def test_missing_build_dependency_fails_before_fetch_or_environment_mutation(self):
        self.recipe('consumer', build=['absent'])
        with self.assertRaises(KeyError):
            self.manager(['consumer'])
        self.assertFalse(self.commands)
        self.p.fetch.assert_not_called()

    def test_invalid_signing_key_fails_before_environment_mutation(self):
        recipe = self.recipe('consumer')
        recipe['keys'] = ['short-key']
        with self.assertRaisesRegex(arch.PreparationError, 'invalid source signing fingerprint'):
            self.manager(['consumer'])
        self.assertFalse(self.commands)

    def test_build_cycles_fail_early_but_runtime_cycles_are_allowed(self):
        self.recipe('one', build=['two'])
        self.recipe('two', build=['one'])
        with self.assertRaisesRegex(arch.PreparationError, 'build dependency cycle'):
            self.manager(['one'])
        self.assertFalse(self.commands)
        self.recipe('one', runtime=['two'])
        self.recipe('two', runtime=['one'])
        self.assertEqual(set(self.manager(['one']).plan.bases), {'one', 'two'})

    def test_build_dependency_runtime_backedge_fails_before_building(self):
        self.recipe('consumer', build=['tool'])
        self.recipe('tool', runtime=['consumer'])
        with self.assertRaisesRegex(arch.PreparationError, 'build dependency cycle'):
            self.manager(['consumer'])
        self.assertFalse(self.commands)

    def test_source_qualified_aur_is_not_replaced_by_official_package(self):
        self.official_package('tool')
        self.recipe('tool')
        manager = self.manager(['aur/tool'])
        self.assertEqual(manager.plan.providers, {'aur/tool': 'tool'})

    def test_prunes_only_obsolete_dependencies_then_updates_official_union_before_building(self):
        self.official_package('compiler', depends=['lib'], version='2-1')
        self.official_package('lib')
        self.recipe('consumer', build=['compiler', 'tool'])
        self.recipe('tool')
        self.installed.update(compiler='1-1', obsolete='1-1', tool='1-1')
        manager = self.manager(['consumer'])
        manager.prepare()
        self.assertNotIn('obsolete', self.installed)
        self.assertEqual(self.installed['compiler'], '2-1')
        self.assertIn('tool', self.installed)
        self.assertIn('lib', self.installed)
        self.assertEqual(self.events[0], ('prune', {'obsolete'}))
        self.assertFalse(manager.fallback)
        self.p.clean_dependencies()
        self.assertIn('compiler', self.installed)
        self.assertIn('tool', self.installed)
        # The next round no longer needs either dependency, even without AUR targets.
        # 下一轮没有 AUR 目标时，这些依赖同样会在构建前清理。
        self.manager([]).prepare()
        self.assertEqual(set(self.installed), set(self.versions))

    def test_official_conflict_warns_and_sleeps_once_then_uses_original_per_target_cleanup(self):
        self.official_package('compiler-one', conflicts=['compiler-two'])
        self.official_package('compiler-two', conflicts=['compiler-one'])
        self.recipe('one', build=['compiler-one'])
        self.recipe('two', build=['compiler-two'])
        self.installed['obsolete'] = '1-1'
        manager = self.manager(['one', 'two'])
        with patch.object(environment.time, 'sleep') as sleep:
            manager.prepare()
        sleep.assert_called_once_with(3)
        self.assertTrue(manager.fallback)
        self.assertEqual(set(self.installed), set(self.versions))
        warnings = [call for call in manager.log.call_args_list if 'Warning:' in call.args[1]]
        self.assertEqual(len(warnings), 1)
        def ensure(target):
            self.installed['compiler-' + target] = '1-1'
        self.p.ensure = Mock(side_effect=ensure)
        manager.build_targets(['one', 'two'])
        self.assertEqual(set(self.installed), set(self.versions))
        self.assertIn('compiler-one', [name for call in self.commands if call[0] == '-R' for name in call])

    def test_download_signature_and_commit_errors_never_trigger_fallback(self):
        self.official_package('compiler')
        self.recipe('consumer', build=['compiler'])
        for diagnostic in ('error: failed retrieving file', 'error: invalid signature',
                           'error: failed to commit transaction (conflicting files)', 'error: hook failed'):
            with self.subTest(diagnostic=diagnostic):
                manager = self.manager(['consumer'])
                original = self.p.pacman.side_effect
                def pacman(args, **kwargs):
                    if args[0] == '-S':
                        return subprocess.CompletedProcess(args, 1, '', diagnostic)
                    return original(args, **kwargs)
                self.p.pacman.side_effect = pacman
                with patch.object(environment.time, 'sleep') as sleep:
                    with self.assertRaisesRegex(arch.PreparationError, 'maintenance failed'):
                        manager.prepare()
                sleep.assert_not_called()
                self.assertFalse(manager.fallback)
                self.p.pacman.side_effect = original

    def test_conflicting_aur_build_dependencies_are_detected_before_any_compilation(self):
        self.recipe('one', build=['tool-one'])
        self.recipe('two', build=['tool-two'])
        self.recipe('tool-one', conflicts=['tool-two'])
        self.recipe('tool-two')
        manager = self.manager(['one', 'two'])
        with patch.object(environment.time, 'sleep') as sleep:
            manager.prepare()
        sleep.assert_called_once_with(3)
        self.assertTrue(manager.fallback)
        self.assertFalse(self.p.artifacts)

    def test_explicit_prepare_conflict_falls_back_but_other_prepare_failures_do_not(self):
        self.recipe('consumer')
        manager = self.manager(['consumer'])
        self.p.pacman.return_value = subprocess.CompletedProcess([], 1, '',
            'error: failed to prepare transaction (could not satisfy dependencies)')
        self.p.pacman.side_effect = None
        with self.assertRaises(environment.DependencyConflict):
            manager.transaction(['-S', '--', 'base'])
        self.p.pacman.return_value.stderr = 'error: failed to prepare transaction (invalid or corrupted database)'
        with self.assertRaises(arch.PreparationError):
            manager.transaction(['-S', '--', 'base'])

    def test_same_aur_artifact_installs_once_for_multiple_requirements_and_survives_next_round(self):
        self.recipe('consumer', build=['tool'])
        self.recipe('tool')
        manager = self.manager(['consumer'])
        artifact = self.artifact('tool')
        manager.install_aur(artifact)
        manager.install_aur(artifact)
        self.assertEqual(sum(call[0] == '-U' for call in self.commands), 1)
        self.assertEqual(self.installed['tool'], '1-1')
        next_round = self.manager(['consumer'])
        next_round.install_aur(artifact)
        self.assertEqual(sum(call[0] == '-U' for call in self.commands), 1)
        # A rebuilt same-version package must replace the older content.
        # 同版本重建产物也必须替换旧内容。
        rebuilt = self.artifact('tool', content='new contents')
        newer_round = self.manager(['consumer'])
        newer_round.install_aur(rebuilt)
        self.assertEqual(sum(call[0] == '-U' for call in self.commands), 2)
        self.assertEqual(json.loads(manager.receipt.read_text())['packages']['tool']['sha256'], rebuilt['sha256'])

    def test_same_version_reinstalled_from_another_source_does_not_reuse_stale_aur_receipt(self):
        self.recipe('consumer', build=['tool'])
        self.recipe('tool')
        first = self.manager(['consumer'])
        artifact = self.artifact('tool')
        first.install_aur(artifact)
        descriptor = self.p.actual_db / 'local/tool-1-1/desc'
        descriptor.write_text('installed by a different source')
        second = self.manager(['consumer'])
        second.install_aur(artifact)
        self.assertEqual(sum(call[0] == '-U' for call in self.commands), 2)

    def test_real_ensure_deduplicates_same_artifact_requested_by_name_and_provides(self):
        self.recipe('consumer', build=['tool'])
        tool = self.recipe('tool', extra='provides = virtual-tool=1\n')
        self.manager(['consumer'])
        artifact = self.artifact('tool')
        artifact['provides'] = ['virtual-tool=1']
        self.p.aur = Mock(return_value=tool)
        self.p.build = Mock()
        self.p.ensure('tool', install=True)
        self.p.ensure('virtual-tool', install=True)
        self.assertEqual(sum(call[0] == '-U' for call in self.commands), 1)

    def test_failed_aur_install_does_not_publish_receipt_or_mark_artifact_installed(self):
        self.recipe('consumer', build=['tool'])
        self.recipe('tool')
        manager = self.manager(['consumer'])
        artifact = self.artifact('tool')
        original = self.p.pacman.side_effect
        self.p.pacman.side_effect = lambda args, **kwargs: (
            subprocess.CompletedProcess(args, 1, '', 'error: invalid signature') if args[0] == '-U'
            else original(args, **kwargs))
        with self.assertRaises(arch.PreparationError):
            manager.install_aur(artifact)
        self.assertFalse(manager.installed)
        self.assertFalse(manager.receipt.exists())

    def test_unexpected_package_version_cannot_escape_database_directory(self):
        self.recipe('consumer')
        manager = self.manager(['consumer'])
        with self.assertRaisesRegex(arch.PreparationError, 'database path'):
            manager.database_identity(dict(name='tool', version='../../elsewhere'))

    def test_aur_dependency_runtime_cycle_installs_as_one_transaction(self):
        self.recipe('consumer', build=['one'])
        self.recipe('one', runtime=['two'])
        self.recipe('two', runtime=['one'])
        manager = self.manager(['consumer'])
        one = self.artifact('one', ['two'])
        self.artifact('two', ['one'])
        manager.install_aur(one)
        transactions = [call for call in self.commands if call[0] == '-U']
        self.assertEqual(len(transactions), 1)
        self.assertEqual({Path(value).name for value in transactions[0][3:]},
                         {self.filename('one'), self.filename('two')})

    def test_late_dependency_conflict_restarts_from_baseline_and_warns_once(self):
        self.recipe('consumer')
        manager = self.manager(['consumer'])
        self.p.ensure = Mock(side_effect=[environment.DependencyConflict('conflicting dependencies'), None])
        self.p.states['consumer'] = 'building'
        self.p.stack.append('consumer')
        with patch.object(environment.time, 'sleep') as sleep:
            manager.build_targets(['consumer'])
        sleep.assert_called_once_with(3)
        self.assertTrue(manager.fallback)
        self.assertEqual(self.p.ensure.call_count, 2)
        self.assertFalse(self.p.states)
        self.assertFalse(self.p.stack)


if __name__ == '__main__':
    unittest.main()

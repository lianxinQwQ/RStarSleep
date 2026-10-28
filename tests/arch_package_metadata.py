"""On-demand AUR discovery and Git synchronization. / 按需 AUR 发现与 Git 同步。"""
import importlib.util
import json
import shutil
import subprocess
import tempfile
import unittest
import urllib.error
import urllib.parse
from io import BytesIO
from pathlib import Path
from unittest.mock import Mock, patch

path = Path(__file__).resolve().parents[1] / 'presets/plugins/arch/lib/predownload.py'
spec = importlib.util.spec_from_file_location('arch_metadata', path)
arch = importlib.util.module_from_spec(spec)
spec.loader.exec_module(arch)


def entry(name, base=None):
    return dict(Name=name, PackageBase=base or name)


def recipe(name, version='1', package=None, extra=''):
    return f'pkgbase = {name}\npkgver = {version}\npkgrel = 1\npkgname = {package or name}\n{extra}'


class MetadataTests(unittest.TestCase):
    def test_official_index_classifies_names_groups_sources_and_versioned_providers(self):
        fields = {'NAME': ['implementation'], 'VERSION': ['2'], 'RSTAR_REPOSITORY': ['extra'],
                  'GROUPS': ['tools'], 'PROVIDES': ['virtual=3', 'bare']}
        index = arch.package_metadata.MetadataIndex({'package': fields})
        satisfies = lambda actual, op, wanted: int(actual) >= int(wanted)
        with patch.object(arch.urllib.request, 'urlopen', side_effect=AssertionError('unexpected network request')):
            for target in ('implementation', 'extra/implementation', 'tools', 'extra/tools', 'virtual>=2', 'bare'):
                self.assertTrue(index.official_target(target, satisfies), target)
            for target in ('core/implementation', 'aur/implementation', 'bare>=1', 'virtual>=4', 'tools>=1'):
                self.assertFalse(index.official_target(target, satisfies), target)

    def test_official_configuration_does_not_query_aur(self):
        p = arch.Preparer.__new__(arch.Preparer)
        p.metadata = arch.package_metadata.MetadataIndex({})
        p.official_target = Mock(return_value=True)
        p.rpc = p.load_base = Mock(side_effect=AssertionError('unexpected AUR operation'))
        p._plan_transaction = Mock(return_value=(['official'], {'official': {'NAME': ['official']}}))
        self.assertEqual(p.collect(['official']), ['official'])
        p.rpc.assert_not_called()


class AurDiscovery(unittest.TestCase):
    def setUp(self):
        self.p = arch.Preparer.__new__(arch.Preparer)
        self.p.bases = {}
        self.responses, self.queries = {}, []
        self.network = patch.object(arch.urllib.request, 'urlopen', side_effect=self.respond).start()
        self.addCleanup(patch.stopall)

    def respond(self, request, **kwargs):
        parsed = urllib.parse.urlsplit(request.full_url)
        self.assertEqual((parsed.scheme, parsed.netloc, parsed.path), ('https', 'aur.archlinux.org', '/rpc/'))
        query = urllib.parse.parse_qs(parsed.query)
        method = query['type'][0]
        name = query['arg[]' if method == 'info' else 'arg'][0]
        if method == 'search':
            self.assertEqual(query['by'], ['provides'])
        self.queries.append((method, name))
        return BytesIO(json.dumps(dict(results=self.responses[(method, name)])).encode())

    def test_split_packages_locate_only_their_base_and_exact_names_precede_providers(self):
        self.responses[('info', 'sample-cli')] = [entry('sample-cli', 'sample')]
        base = arch.srcinfo(recipe('sample', package='sample-cli') + 'pkgname = sample-gui\n', 'x86_64')
        self.p.load_base = Mock(return_value=base)
        self.assertIs(self.p.aur('aur/sample-cli'), base)
        self.assertIs(self.p.aur('sample-cli'), base)
        self.assertEqual(self.queries, [('info', 'sample-cli')])
        self.assertEqual({call.args[0] for call in self.p.load_base.call_args_list}, {'sample'})

    @unittest.skipUnless(shutil.which('vercmp'), 'vercmp unavailable / 缺少 vercmp')
    def test_versioned_providers_use_recipe_data_and_recheck_each_constraint(self):
        self.responses[('info', 'virtual')] = []
        self.responses[('search', 'virtual')] = [entry('provider-a', 'provider'), entry('provider-b', 'provider'), entry('wrong')]
        recipes = {
            'provider': arch.srcinfo(recipe('provider', extra='provides = virtual=2\n'), 'x86_64'),
            'wrong': arch.srcinfo(recipe('wrong', extra='provides = other=9\n'), 'x86_64'),
        }
        self.p.load_base = Mock(side_effect=recipes.__getitem__)
        self.assertEqual(self.p.aur('virtual>=2')['name'], 'provider')
        with self.assertRaisesRegex(arch.PreparationError, 'missing or ambiguous AUR provider'):
            self.p.aur('virtual>=3')
        self.assertEqual(self.queries, [('info', 'virtual'), ('search', 'virtual')])
        self.assertEqual({call.args[0] for call in self.p.load_base.call_args_list}, {'provider', 'wrong'})

    @unittest.skipUnless(shutil.which('vercmp'), 'vercmp unavailable / 缺少 vercmp')
    def test_outdated_mirror_recipe_cannot_satisfy_newer_exact_package(self):
        self.responses[('info', 'sample')] = [dict(entry('sample'), Version='2-1')]
        self.p.load_base = Mock(return_value=arch.srcinfo(recipe('sample'), 'x86_64'))
        with self.assertRaisesRegex(arch.PreparationError, 'missing or ambiguous AUR provider'):
            self.p.aur('sample>=2')
        self.assertEqual(self.queries, [('info', 'sample')])

    def test_runtime_dependency_closure_discovers_only_required_bases(self):
        recipes = {
            'application': arch.srcinfo('pkgbase = application\npkgver = 1\npkgrel = 1\n'
                                       'makedepends = compiler\npkgname = application-cli\ndepends = middle\n', 'x86_64'),
            'middle': arch.srcinfo(recipe('middle', extra='depends = virtual\n'), 'x86_64'),
            'provider': arch.srcinfo(recipe('provider', extra='provides = virtual\n'), 'x86_64'),
        }
        self.responses.update({('info', 'application-cli'): [entry('application-cli', 'application')],
                               ('info', 'middle'): [entry('middle')], ('info', 'virtual'): [],
                               ('search', 'virtual'): [entry('provider')]})
        self.p.load_base = Mock(side_effect=recipes.__getitem__)
        self.p.metadata = arch.package_metadata.MetadataIndex({})
        self.p.official_target = Mock(return_value=False)
        self.p._plan_transaction = Mock(return_value=(['application-cli', 'middle', 'provider'],
                                                      {name: {'NAME': [name]} for name in ('application-cli', 'middle', 'provider')}))
        self.assertEqual(self.p.collect(['aur/application-cli']), ['application-cli', 'middle', 'provider'])
        self.assertEqual(self.queries, [('info', 'application-cli'), ('info', 'middle'), ('info', 'virtual'), ('search', 'virtual')])
        self.assertEqual({call.args[0] for call in self.p.load_base.call_args_list}, set(recipes))

    def test_missing_package_and_empty_search_are_cached_only_within_invocation(self):
        self.responses.update({('info', 'missing'): [], ('search', 'missing'): []})
        self.p.load_base = Mock(side_effect=AssertionError('unexpected recipe fetch'))
        for _ in range(2):
            with self.assertRaisesRegex(arch.PreparationError, 'missing or ambiguous AUR provider'):
                self.p.aur('missing')
        self.assertEqual(self.queries, [('info', 'missing'), ('search', 'missing')])
        self.responses[('info', 'missing')] = [entry('missing')]
        new = arch.Preparer.__new__(arch.Preparer)
        self.assertEqual(new.rpc('info', 'missing'), [entry('missing')])
        self.assertEqual(len(self.queries), 3)

    def test_invalid_rpc_responses_fail_without_fetching_recipes(self):
        invalid = [[], {}, {'results': None}, {'error': 'unavailable', 'results': []},
                   {'results': [entry('different')]}, {'results': [entry('sample', '../escape')]},
                   {'results': [entry('sample'), entry('sample')]}, {'results': ['invalid']}]
        for payload in invalid:
            with self.subTest(payload=payload):
                p = arch.Preparer.__new__(arch.Preparer)
                with patch.object(arch.urllib.request, 'urlopen', return_value=BytesIO(json.dumps(payload).encode())):
                    with self.assertRaises(arch.PreparationError) as error:
                        p.aur('sample')
                self.assertIn(' / ', str(error.exception))
                self.assertFalse(p.aur_queries)

    def test_rpc_failure_does_not_use_cached_recipes_or_other_sources(self):
        self.p.bases = {'provider': arch.srcinfo(recipe('provider', extra='provides = virtual\n'), 'x86_64')}
        self.p.load_base = Mock(side_effect=AssertionError('stale recipe must not be used'))
        self.network.side_effect = urllib.error.URLError('offline / 离线')
        with self.assertRaisesRegex(arch.PreparationError, 'AUR RPC failed for virtual'):
            self.p.aur('virtual')
        self.p.load_base.assert_not_called()
        self.assertEqual(self.network.call_count, 1)
        self.assertFalse(self.p.aur_queries)


@unittest.skipUnless(shutil.which('git'), 'git unavailable / 缺少 git')
class AurGit(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.remote = self.root / 'remote'
        self.checkouts = self.root / 'checkouts'
        self.checkouts.mkdir()
        self.git('init', '--quiet', '--initial-branch=main', str(self.remote))
        self.git('-C', str(self.remote), 'config', 'user.name', 'Offline Test')
        self.git('-C', str(self.remote), 'config', 'user.email', 'offline@example.invalid')
        (self.remote / 'README.md').write_text('mirror / 镜像\n')
        self.commit('main')
        self.git('-C', str(self.remote), 'checkout', '--orphan', 'sample')
        self.git('-C', str(self.remote), 'rm', '-rf', '.')
        self.write_recipe('1')
        self.previous = self.commit('previous sample')
        self.write_recipe('2')
        self.current = self.commit('current sample')
        self.git('-C', str(self.remote), 'tag', 'sample-tag')
        self.git('-C', str(self.remote), 'checkout', '--orphan', 'unrelated')
        self.git('-C', str(self.remote), 'rm', '-rf', '.')
        (self.remote / '.SRCINFO').write_text(recipe('unrelated'))
        (self.remote / 'PKGBUILD').write_text('unrelated / 无关配方\n')
        self.unrelated = self.commit('unrelated')
        self.git('-C', str(self.remote), 'tag', 'unrelated-tag')
        self.commands = []
        remote_patch = patch.object(arch, 'AUR_GIT_URL', self.remote.as_uri())
        remote_patch.start()
        self.addCleanup(remote_patch.stop)

    def git(self, *arguments):
        result = subprocess.run(['git', *arguments], text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        if result.returncode:
            self.fail(f'Git 测试命令失败 / Git test command failed: {arguments}\n{result.stderr}')
        return result.stdout.strip()

    def commit(self, message):
        self.git('-C', str(self.remote), 'add', '.')
        self.git('-C', str(self.remote), 'commit', '-qm', message)
        return self.git('-C', str(self.remote), 'rev-parse', 'HEAD')

    def write_recipe(self, version):
        (self.remote / '.SRCINFO').write_text(recipe('sample', version, 'sample-cli') + 'pkgname = sample-gui\n')
        (self.remote / 'PKGBUILD').write_text(f'pkgbase=sample\npkgname=(sample-cli sample-gui)\npkgver={version}\npkgrel=1\narch=(x86_64)\n')

    def preparer(self):
        p = arch.Preparer.__new__(arch.Preparer)
        p.checkouts, p.arch, p.bases = self.checkouts, 'x86_64', {}
        def user(arguments, **kwargs):
            self.commands.append(list(arguments))
            return arch.run(arguments, capture=True)
        p.user = user
        def printsrcinfo(checkout, arguments, **kwargs):
            self.assertEqual(arguments, ['--printsrcinfo'])
            return subprocess.CompletedProcess([], 0, (checkout / '.SRCINFO').read_text(), '')
        p.makepkg = Mock(side_effect=printsrcinfo)
        return p

    def test_only_requested_branch_tip_is_fetched_without_tags(self):
        p = self.preparer()
        base = p.load_base('sample')
        checkout = self.checkouts / 'sample'
        self.assertEqual((base['commit'], base['version']), (self.current, '2-1'))
        self.assertEqual(self.git('-C', str(checkout), 'for-each-ref', '--format=%(refname)', 'refs/remotes/'),
                         'refs/remotes/origin/sample')
        self.assertEqual(self.git('-C', str(checkout), 'rev-list', '--count', 'HEAD'), '1')
        self.assertEqual(self.git('-C', str(checkout), 'tag'), '')
        for revision in (self.previous, self.unrelated):
            result = subprocess.run(['git', '-C', str(checkout), 'cat-file', '-e', revision], capture_output=True)
            self.assertNotEqual(result.returncode, 0)
        fetches = [command for command in self.commands if 'fetch' in command]
        self.assertEqual(len(fetches), 1)
        self.assertEqual(fetches[0][-1], '+refs/heads/sample:refs/remotes/origin/sample')
        self.assertFalse(any('*' in arg or arg in ('pull', '--all', '--mirror') for command in self.commands for arg in command))
        self.assertIs(p.load_base('sample'), base)
        self.assertEqual(len([command for command in self.commands if 'fetch' in command]), 1)

    def test_subsequent_calls_update_only_requested_cached_bases(self):
        first = self.preparer()
        first.load_base('sample')
        first.load_base('unrelated')
        self.git('-C', str(self.remote), 'checkout', 'sample')
        self.write_recipe('3')
        updated = self.commit('updated sample')
        self.git('-C', str(self.remote), 'checkout', 'unrelated')
        (self.remote / 'untouched').write_text('new unrelated content / 新的无关内容\n')
        self.commit('updated unrelated')
        self.commands.clear()
        second = self.preparer()
        self.assertEqual(second.load_base('sample')['commit'], updated)
        self.assertEqual(self.git('-C', str(self.checkouts / 'unrelated'), 'rev-parse', 'HEAD'), self.unrelated)
        self.assertEqual([command[-1] for command in self.commands if 'fetch' in command],
                         ['+refs/heads/sample:refs/remotes/origin/sample'])
        self.assertEqual(first.load_base('sample')['commit'], self.current)

    def test_legacy_checkout_migrates_origin_and_removes_all_wildcard_fetch_rules(self):
        checkout = self.checkouts / 'sample'
        self.git('clone', '--quiet', '--single-branch', '--branch', 'sample', self.remote.as_uri(), str(checkout))
        self.git('-C', str(checkout), 'remote', 'set-url', 'origin', 'https://aur.archlinux.org/sample.git')
        self.git('-C', str(checkout), 'config', '--add', 'remote.origin.fetch', '+refs/heads/*:refs/remotes/origin/*')
        self.git('-C', str(checkout), 'config', '--add', 'remote.origin.fetch', '+refs/tags/*:refs/tags/*')
        (checkout / 'stale-build-file').write_text('discard / 清理\n')
        self.assertEqual(self.preparer().load_base('sample')['commit'], self.current)
        self.assertEqual(self.git('-C', str(checkout), 'remote', 'get-url', 'origin'), self.remote.as_uri())
        self.assertEqual(self.git('-C', str(checkout), 'config', '--get-all', 'remote.origin.fetch'),
                         '+refs/heads/sample:refs/remotes/origin/sample')
        self.assertFalse((checkout / 'stale-build-file').exists())

    def test_deleted_branch_fails_without_parsing_or_resetting_old_checkout(self):
        self.preparer().load_base('sample')
        self.git('-C', str(self.remote), 'update-ref', '-d', 'refs/heads/sample')
        self.commands.clear()
        p = self.preparer()
        with self.assertRaisesRegex(arch.PreparationError, 'command failed') as error:
            p.load_base('sample')
        self.assertIn('命令执行失败', str(error.exception))
        p.makepkg.assert_not_called()
        self.assertNotIn('sample', p.bases)
        self.assertFalse(any('reset' in command or 'clean' in command for command in self.commands))
        self.assertEqual(self.git('-C', str(self.checkouts / 'sample'), 'rev-parse', 'HEAD'), self.current)

    def test_force_updated_branch_is_reset_to_the_fetched_commit(self):
        self.preparer().load_base('sample')
        self.git('-C', str(self.remote), 'update-ref', 'refs/heads/sample', self.previous)
        self.assertEqual(self.preparer().load_base('sample')['commit'], self.previous)

    def test_checkout_symlink_is_rejected_before_git_commands(self):
        (self.checkouts / 'sample').symlink_to(self.remote, target_is_directory=True)
        with self.assertRaisesRegex(arch.PreparationError, 'must not be a symlink'):
            self.preparer().load_base('sample')
        self.assertFalse(self.commands)
        self.assertTrue((self.remote / 'PKGBUILD').exists())

    def test_invalid_branch_name_fails_before_creating_checkout(self):
        for name in ('sample..invalid', 'sample.lock', 'sample.'):
            with self.subTest(name=name), self.assertRaises(arch.PreparationError):
                self.preparer().load_base(name)
            self.assertFalse((self.checkouts / name).exists())
        self.assertTrue(all(command[1] == 'check-ref-format' for command in self.commands))


if __name__ == '__main__':
    unittest.main()

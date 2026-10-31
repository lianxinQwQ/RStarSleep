#!/usr/bin/env python3
"""Account merge tests use isolated tables, never host account tools."""
import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest

PLUGIN = Path(__file__).resolve().parents[1] / 'presets/plugins/arch'


class Accounts(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.work = self.root / 'work'
        for name in ('old', 'new', 'output'):
            (self.work / name / 'etc').mkdir(parents=True)
        (self.root / 'target/etc').mkdir(parents=True)
        self.write('old', 'passwd', 'root:x:0:0:root:/root:/bin/bash\nalice:x:1000:1000:Alice:/home/alice:/bin/bash\nretired:x:1001:1001::/home/retired:/bin/sh\n')
        self.write('old', 'group', 'root:x:0:\nalice:x:1000:\nretired:x:1001:\nwheel:x:10:alice,retired\n')
        self.write('old', 'shadow', 'root:!:::::::\nalice:$6$private:20000:0:99999:7:::\nretired:!:::::::\n')
        self.write('old', 'gshadow', 'root:!::\nalice:!::\nretired:!::\nwheel:!::alice,retired\n')
        self.write('new', 'passwd', 'root:x:0:0:root:/root:/bin/bash\nservice:x:900:900::/var/lib/service:/usr/bin/nologin\n')
        self.write('new', 'group', 'root:x:0:\nservice:x:900:\nwheel:x:10:service\n')
        self.write('new', 'shadow', 'root:!:::::::\nservice:!:::::::\n')
        self.write('new', 'gshadow', 'root:!::\nservice:!::\nwheel:!::service\n')
        self.request(users=['alice'])

    def tearDown(self):
        self.temporary.cleanup()

    def write(self, tree, name, text):
        (self.work / tree / 'etc' / name).write_text(text)

    def request(self, users=None, groups=None):
        (self.work / 'request.json').write_text(json.dumps(dict(schema_version=1, users=users or [], groups=groups or [])))

    def run_action(self, operation, preflight=False, success=True):
        import os
        env = dict(os.environ, RSTAR_PLUGIN_ROOT=str(PLUGIN), RSTAR_ROOT=str(self.root / 'target'))
        args = ['bash', str(PLUGIN / ('account_' + operation + '.sh')), '--workspace', str(self.work)]
        if preflight:
            args += ['--preflight']
        result = subprocess.run(args, env=env, capture_output=True, text=True)
        self.assertEqual(result.returncode == 0, success, result.stderr)
        self.assertNotIn('$6$private', result.stdout + result.stderr)
        return json.loads(result.stdout) if result.stdout else None

    def test_preflight_before_target_and_missing_selection(self):
        result = self.run_action('check', True)
        self.assertEqual(result['dropped_users'], ['retired', 'root'])
        self.assertNotIn('alice', result['dropped_groups'])
        self.request(users=['missing', 'another', 'missing'])
        result = self.run_action('check', True)
        self.assertFalse(result['consistent'])
        self.assertEqual(result['missing_users'], ['another', 'missing'])
        self.assertTrue(all('selected source user does not exist' in item for item in result['differences']))
        self.run_action('prepare', success=False)
        self.assertFalse(any((self.work / 'output/etc').iterdir()))

    def test_merge_preserves_new_accounts_primary_group_and_memberships(self):
        self.run_action('prepare')
        result = self.run_action('check')
        self.assertEqual(result['added_users'], ['service'])
        self.assertEqual(result['dropped_users'], ['retired'])
        self.assertEqual(result['differences'], [])
        self.assertEqual(result['drifts'], [])
        self.assertIn('wheel:x:10:alice,service', (self.work / 'output/etc/group').read_text())
        self.run_action('apply')
        self.assertIn('$6$private', (self.root / 'target/etc/shadow').read_text())
        self.assertIn('service:x:900', (self.root / 'target/etc/passwd').read_text())
        self.assertNotIn('retired:', (self.root / 'target/etc/passwd').read_text())

    def test_id_collision_reported_without_chown_or_remapping(self):
        path = self.work / 'new/etc/passwd'
        path.write_text(path.read_text().replace('service:x:900:900', 'service:x:1000:900'))
        self.run_action('prepare')
        result = self.run_action('check')
        self.assertTrue(any(d['name'] == 'alice' and d['new_name'] == 'service' and d['old_id'] == 1000 for d in result['drifts']))
        self.assertIn('alice:x:1000:1000', (self.work / 'output/etc/passwd').read_text())
        self.assertIn('service:x:1000:900', (self.work / 'output/etc/passwd').read_text())

    def test_unselected_same_name_drift_and_semantic_order(self):
        self.request()
        path = self.work / 'new/etc/passwd'
        path.write_text(path.read_text() + 'alice:x:2000:900:Alice:/home/alice:/bin/bash\n')
        self.run_action('prepare')
        result = self.run_action('check')
        self.assertTrue(any(d['name'] == 'alice' and d['new_id'] == 2000 for d in result['drifts']))
        for filename in ('passwd', 'group', 'shadow', 'gshadow'):
            path = self.work / 'output/etc' / filename
            path.write_text('\n'.join(reversed(path.read_text().splitlines())) + '\n')
        self.assertEqual(result, self.run_action('check'))

    def test_selected_existing_build_ids_are_not_remapped(self):
        path = self.work / 'new/etc/passwd'
        path.write_text(path.read_text() + 'alice:x:2000:2000:New:/other:/bin/sh\n')
        path = self.work / 'new/etc/group'
        path.write_text(path.read_text() + 'alice:x:2000:\n')
        self.run_action('prepare')
        self.assertIn('alice:x:2000:2000:Alice:/home/alice:/bin/bash', (self.work / 'output/etc/passwd').read_text())
        self.assertIn('alice:x:2000:', (self.work / 'output/etc/group').read_text())
        result = self.run_action('check')
        self.assertEqual(result['differences'], [])
        self.assertEqual({(d['kind'], d['old_id'], d['new_id']) for d in result['drifts']}, {('uid', 1000, 2000), ('gid', 1000, 2000)})

    def test_group_selection_does_not_create_all_members(self):
        self.request(groups=['wheel'])
        self.run_action('prepare')
        self.assertNotIn('retired:', (self.work / 'output/etc/passwd').read_text())
        self.assertIn('wheel:x:10:service', (self.work / 'output/etc/group').read_text())

    def test_check_rejects_undeclared_accounts_and_credential_changes(self):
        self.run_action('prepare')
        path = self.work / 'output/etc/passwd'
        path.write_text(path.read_text() + 'retired:x:1001:1001::/home/retired:/bin/sh\n')
        self.assertTrue(any('undeclared account' in d for d in self.run_action('check')['differences']))
        self.run_action('prepare')
        path = self.work / 'output/etc/shadow'
        path.write_text(path.read_text().replace('root:!', 'root:$6$private'))
        self.assertTrue(any('undeclared credential' in d for d in self.run_action('check')['differences']))

    def test_duplicate_records_and_symlink_targets_rejected(self):
        path = self.work / 'old/etc/passwd'
        path.write_text(path.read_text() + 'alice:x:2000:1000::/home/alice:/bin/sh\n')
        self.run_action('check', True, success=False)
        path.write_text(path.read_text().rsplit('alice:x:2000', 1)[0])
        self.run_action('prepare')
        victim = self.root / 'victim'
        victim.write_text('unchanged')
        (self.root / 'target/etc/shadow').symlink_to(victim)
        self.run_action('apply', success=False)
        self.assertEqual(victim.read_text(), 'unchanged')
        self.assertFalse((self.root / 'target/etc/passwd').exists())

    def test_optional_shadow_absence_is_preserved(self):
        for tree in ('old', 'new'):
            for filename in ('shadow', 'gshadow'):
                (self.work / tree / 'etc' / filename).unlink()
        self.run_action('prepare')
        self.assertFalse((self.work / 'output/etc/shadow').exists())
        self.assertEqual(self.run_action('check')['differences'], [])


if __name__ == '__main__':
    unittest.main()

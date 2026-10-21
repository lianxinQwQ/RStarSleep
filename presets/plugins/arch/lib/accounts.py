#!/usr/bin/env python3
"""Selected local account inheritance; no UID/GID repair or application-data edits."""
import argparse
import json
import os
from pathlib import Path
import stat
import sys
import tempfile

FILES = {'passwd': 7, 'group': 4, 'shadow': 9, 'gshadow': 4}


def safe(path):
    path = Path(path)
    if not path.is_absolute():
        raise ValueError('account paths must be absolute')
    for item in (path, *path.parents):
        if item.is_symlink():
            raise ValueError('account paths must not traverse symbolic links')
    return path


def number(value):
    if not value.isascii() or not value.isdigit() or not 0 <= int(value) < 2**32 - 1:
        raise ValueError('invalid numeric account ID')
    return int(value)


def members(value):
    return set(value.split(',')) if value else set()


def load(tree):
    result = {}
    for name, width in FILES.items():
        path = safe(tree / 'etc' / name)
        if not path.exists():
            if name in ('passwd', 'group'):
                raise ValueError('missing required account file: ' + name)
            result[name] = None
            continue
        if not path.is_file():
            raise ValueError('account input is not a regular file: ' + name)
        table = {}
        try:
            lines = path.read_text().splitlines()
        except UnicodeError:
            raise ValueError('invalid account text encoding: ' + name) from None
        for line in lines:
            if not line:
                continue
            fields = line.split(':')
            if '\x00' in line or len(fields) != width or not fields[0] or fields[0] in table:
                raise ValueError('invalid or duplicate record in ' + name)
            if any(c.isspace() or c in '/,\\' for c in fields[0]):
                raise ValueError('invalid account name in ' + name)
            if name in ('passwd', 'group'):
                number(fields[2])
            if name == 'passwd':
                number(fields[3])
            if name in ('group', 'gshadow'):
                fields[-1] = ','.join(sorted(members(fields[-1])))
            if name == 'gshadow':
                fields[2] = ','.join(sorted(members(fields[2])))
            table[fields[0]] = fields
        result[name] = table
    return result


def selection(old, request):
    users = set(request['users'])
    groups = set(request['groups'])
    for name in users:
        if name not in old['passwd']:
            raise ValueError('selected source user does not exist: ' + name)
        gid = old['passwd'][name][3]
        primary = [g for g, fields in old['group'].items() if fields[2] == gid]
        if not primary:
            raise ValueError('selected source user has no local primary group: ' + name)
        groups.update(primary)
        if old['shadow'] is not None and old['passwd'][name][1] == 'x' and name not in old['shadow']:
            raise ValueError('selected source user has no shadow record: ' + name)
    for name in groups:
        if name not in old['group']:
            raise ValueError('selected source group does not exist: ' + name)
    return users, groups


def write_table(path, table, mode):
    safe(path)
    owner = path.stat() if path.exists() else None
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    with tempfile.NamedTemporaryFile(dir=path.parent, prefix='.rstar-accounts-', delete=False) as handle:
        temporary = Path(handle.name)
        try:
            if owner is not None:
                os.fchown(handle.fileno(), owner.st_uid, owner.st_gid)
            os.fchmod(handle.fileno(), mode)
            handle.write(''.join(':'.join(row) + '\n' for row in table.values()).encode())
            handle.flush()
            os.fsync(handle.fileno())
            os.replace(temporary, path)
        finally:
            temporary.unlink(missing_ok=True)


def prepare(work, old, new, request):
    users, groups = selection(old, request)
    result = {name: None if table is None else {key: row[:] for key, row in table.items()} for name, table in new.items()}
    for name in users:
        result['passwd'][name] = old['passwd'][name][:]
        if old['shadow'] is not None and name in old['shadow']:
            if result['shadow'] is None:
                result['shadow'] = {}
            result['shadow'][name] = old['shadow'][name][:]
    for name in groups:
        result['group'][name] = old['group'][name][:]
        if name in new['group']:
            result['group'][name][2] = new['group'][name][2]
        if old['gshadow'] is not None and name in old['gshadow']:
            if result['gshadow'] is None:
                result['gshadow'] = {}
            result['gshadow'][name] = old['gshadow'][name][:]
    for name in users:
        # Existing build identities keep their numbers. Never remap build-owned files.
        if name in new['passwd']:
            result['passwd'][name][2] = new['passwd'][name][2]
        gid = old['passwd'][name][3]
        primary = sorted(g for g, row in old['group'].items() if row[2] == gid)[0]
        result['passwd'][name][3] = result['group'][primary][2]
    available = set(result['passwd'])
    for name, row in result['group'].items():
        # Keep build-produced memberships and import only explicitly selected relationships.
        current = members(new['group'].get(name, ['', '', '', ''])[3])
        source = members(old['group'].get(name, ['', '', '', ''])[3])
        inherited = source if name in groups else source & users
        row[3] = ','.join(sorted((current | inherited) & available))
        if result['gshadow'] is not None:
            entry = result['gshadow'].setdefault(name, [name, '!', '', ''])
            admins = members((new['gshadow'] or {}).get(name, [name, '!', '', ''])[2])
            old_admins = members((old['gshadow'] or {}).get(name, [name, '!', '', ''])[2])
            entry[2] = ','.join(sorted((admins | (old_admins if name in groups else old_admins & users)) & available))
            entry[3] = row[3]
    for filename, table in result.items():
        if table is None:
            continue
        original = work / 'new/etc' / filename
        mode = stat.S_IMODE(original.stat().st_mode) if original.exists() else (0o600 if filename in ('shadow', 'gshadow') else 0o644)
        write_table(work / 'output/etc' / filename, table, mode)


def report(old, candidate, request, new=None):
    users, groups = selection(old, request)
    result = dict(schema_version=1, consistent=True, added_users=[], added_groups=[], dropped_users=[], dropped_groups=[], drifts=[], differences=[])
    for kind, filename, selected in [('uid', 'passwd', users), ('gid', 'group', groups)]:
        source, target = old[filename], candidate[filename]
        noun = 'users' if kind == 'uid' else 'groups'
        result['dropped_' + noun] = sorted(set(source) - set(target))
        result['added_' + noun] = sorted(set(target) - set(source))
        by_id = {}
        for name, row in target.items():
            by_id.setdefault(number(row[2]), []).append(name)
        for name, row in source.items():
            old_id = number(row[2])
            after = target.get(name)
            if after is not None and number(after[2]) != old_id:
                result['drifts'].append(dict(kind=kind, name=name, old_id=old_id, new_id=number(after[2]), new_name=name))
            for other in by_id.get(old_id, []):
                if other != name and (other not in source or number(source[other][2]) != old_id):
                    result['drifts'].append(dict(kind=kind, name=name, old_id=old_id, new_id=old_id, new_name=other))
        # Selected identities must be preserved exactly, except separately merged memberships.
        for name in selected:
            expected = source[name][:]
            if new is not None and name in new[filename]:
                expected[2] = new[filename][name][2]
            if filename == 'passwd':
                primary = sorted(g for g, row in old['group'].items() if row[2] == source[name][3])[0]
                expected[3] = candidate['group'][primary][2] if primary in candidate['group'] else source[name][3]
            actual = target.get(name)
            comparable = slice(None) if filename == 'passwd' else slice(0, 3)
            if actual is None or expected[comparable] != actual[comparable]:
                result['differences'].append('selected account was not preserved: ' + name)
        if new is not None:
            for name, actual in target.items():
                if name not in selected:
                    before = new[filename].get(name)
                    comparable = slice(None) if filename == 'passwd' else slice(0, 3)
                    if before is None or actual[comparable] != before[comparable]:
                        result['differences'].append('undeclared account change: ' + name)
            for name in set(target) - set(source):
                if name not in new[filename]:
                    result['differences'].append('output introduced an account absent from the build: ' + name)
    for name in users:
        expected = (old['shadow'] or {}).get(name)
        if expected is not None and (candidate['shadow'] or {}).get(name) != expected:
            result['differences'].append('selected credential was not preserved: ' + name)
    if new is not None:
        for name, row in (candidate['shadow'] or {}).items():
            if name not in users and row != (new['shadow'] or {}).get(name):
                result['differences'].append('undeclared credential change: ' + name)
        for name, row in candidate['group'].items():
            source = members(old['group'].get(name, [name, '', '', ''])[3])
            built = members(new['group'].get(name, [name, '', '', ''])[3])
            inherited = source if name in groups else source & users
            expected = (built | inherited) & set(candidate['passwd'])
            if members(row[3]) != expected:
                result['differences'].append('unexpected group membership: ' + name)
        for name in groups:
            expected = (old['gshadow'] or {}).get(name)
            actual = (candidate['gshadow'] or {}).get(name)
            if expected is not None and (actual is None or actual[1] != expected[1]):
                result['differences'].append('selected group credential was not preserved: ' + name)
    for name, row in candidate['passwd'].items():
        if not any(g[2] == row[3] for g in candidate['group'].values()):
            result['differences'].append('output user has no local primary group: ' + name)
    if candidate['gshadow'] is not None:
        for name, row in candidate['group'].items():
            entry = candidate['gshadow'].get(name)
            if entry is None or members(row[3]) != members(entry[3]):
                result['differences'].append('group and gshadow membership disagree: ' + name)
            if new is not None and entry is not None:
                source = members((old['gshadow'] or {}).get(name, [name, '!', '', ''])[2])
                built = members((new['gshadow'] or {}).get(name, [name, '!', '', ''])[2])
                inherited = source if name in groups else source & users
                if members(entry[2]) != (built | inherited) & set(candidate['passwd']):
                    result['differences'].append('unexpected group administrators: ' + name)
    result['consistent'] = not any(result[key] for key in ('dropped_users', 'dropped_groups', 'drifts', 'differences'))
    return result


def apply(work, root):
    candidate = load(work / 'output')
    targets = []
    # Validate every destination before changing any account file in the staging root.
    for name, table in candidate.items():
        target = safe(root / 'etc' / name)
        if target.exists() and not target.is_file():
            raise ValueError('account destination is not a regular file')
        targets.append((name, table, target))
    for name, table, target in targets:
        if table is None:
            target.unlink(missing_ok=True)
            continue
        mode = stat.S_IMODE((work / 'output/etc' / name).stat().st_mode)
        write_table(target, table, mode)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('operation', choices=['prepare', 'check', 'apply'])
    parser.add_argument('--workspace', required=True, type=Path)
    parser.add_argument('--preflight', action='store_true')
    args = parser.parse_args()
    work = safe(args.workspace)
    request = json.loads(safe(work / 'request.json').read_text())
    if request.get('schema_version') != 1 or any(not isinstance(request.get(k), list) or any(not isinstance(n, str) for n in request[k]) for k in ('users', 'groups')):
        raise ValueError('unsupported account request')
    old = load(work / 'old')
    users, groups = selection(old, request)
    if args.preflight:
        if args.operation != 'check':
            raise ValueError('only check supports preflight')
        # A candidate-loss warning, not a claim about an unbuilt system.
        result = dict(schema_version=1, consistent=True, added_users=[], added_groups=[],
                      dropped_users=sorted(set(old['passwd']) - users), dropped_groups=sorted(set(old['group']) - groups), drifts=[], differences=[])
        print(json.dumps(result))
    elif args.operation == 'prepare':
        prepare(work, old, load(work / 'new'), request)
    elif args.operation == 'check':
        print(json.dumps(report(old, load(work / 'output'), request, load(work / 'new'))))
    else:
        apply(work, safe(Path(os.environ['RSTAR_ROOT'])))


if __name__ == '__main__':
    try:
        main()
    except (ValueError, OSError, KeyError) as error:
        # Exceptions never include account record contents or password hashes.
        print('account operation failed: ' + (str(error) if isinstance(error, ValueError) else type(error).__name__), file=sys.stderr)
        sys.exit(1)

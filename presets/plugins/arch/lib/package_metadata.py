"""Index local official repositories. / 索引本地官方仓库。"""
import re

DEPENDENCY = re.compile(r'^([A-Za-z0-9@_+][A-Za-z0-9@._+-]*)(?:(>=|<=|=|>|<)([^\s]+))?$')


class MetadataIndex:
    def __init__(self, official):
        self.names, self.groups, self.provides = {}, {}, {}
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

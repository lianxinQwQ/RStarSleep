"""Preparation must reuse copied databases and perform legacy permission migration once."""
import os
import subprocess
import tempfile
import unittest
from pathlib import Path

class PreparationReuse(unittest.TestCase):
    def test_database_copy_and_permission_migration(self):
        helper=Path(__file__).resolve().parents[1]/'presets/plugins/arch/lib/common.sh'
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory)
            (root/'cache').mkdir()
            (root/'cache/a.pkg.tar.zst').write_bytes(b'package')
            (root/'cache/a.pkg.tar.zst').chmod(0o600)
            (root/'source').write_bytes(b'repository')
            script='''source "$1"
cd "$2"
prepare_shared_cache cache
[[ $(stat -c %a cache/a.pkg.tar.zst) == 644 ]]
chmod() { echo unexpected-repeat-chmod >&2; return 77; }
prepare_shared_cache cache
copy_prepared_database source destination
inode=$(stat -c %i destination)
copy_prepared_database source destination
[[ $(stat -c %i destination) == "$inode" ]]
printf changed > source
copy_prepared_database source destination
[[ $(<destination) == changed ]]
'''
            result=subprocess.run(['bash','-eu','-c',script,'reuse-test',str(helper),str(root)],capture_output=True,text=True)
            self.assertEqual(result.returncode,0,result.stderr)

if __name__=='__main__':unittest.main()

"""An explicit source receipt must notice target, version and local reinstallation changes."""
import os
import subprocess
import tempfile
import unittest
from pathlib import Path

class SourceReuse(unittest.TestCase):
    def test_source_receipts_invalidate_on_changes(self):
        helper=Path(__file__).resolve().parents[1]/'presets/plugins/arch/lib/maintenance.sh'
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory)
            (root/'var/lib/pacman/local/vim-1-1').mkdir(parents=True)
            (root/'var/lib/pacman/local/vim-1-1/desc').write_text('original descriptor')
            script='''source "$1"
export RSTAR_ROOT=$2
pacman() {
    if [[ $1 == -Q ]]; then printf 'vim 1-1\\n'; else printf 'Name : vim\\nVersion : 1-1\\n'; fi
}
! source_target_matches vim custom/vim
save_source_target vim custom/vim
source_target_matches vim custom/vim
! source_target_matches vim other/vim
printf modified > "$RSTAR_ROOT/var/lib/pacman/local/vim-1-1/desc"
! source_target_matches vim custom/vim
save_source_target vim custom/vim
source_target_matches vim custom/vim
'''
            result=subprocess.run(['bash','-eu','-c',script,'source-test',str(helper),str(root)],capture_output=True,text=True)
            self.assertEqual(result.returncode,0,result.stderr)

if __name__=='__main__':unittest.main()

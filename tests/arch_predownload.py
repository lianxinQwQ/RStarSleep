"""Protocol tests with no package-manager, network or host-system mutations."""
import importlib.util
import os
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import Mock, patch

module_path = Path(__file__).resolve().parents[1] / "presets/plugins/arch/lib/predownload.py"
module_spec = importlib.util.spec_from_file_location("arch_predownload", module_path)
arch = importlib.util.module_from_spec(module_spec)
module_spec.loader.exec_module(arch)

RECIPE = """pkgbase = sample
pkgver = 1
pkgrel = 1
arch = x86_64
depends = common
makedepends = compiler
pkgname = sample-cli
pkgname = sample-gui
depends = gui-library
provides = virtual-gui=1
"""


class Protocol(unittest.TestCase):
    def test_split_dependency_override(self):
        recipe = arch.srcinfo(RECIPE, "x86_64")
        self.assertEqual(recipe["runtime"], ["common", "gui-library"])
        self.assertEqual(recipe["build_runtime"], ["common"])
        self.assertEqual(recipe["build"], ["compiler"])
        self.assertEqual(set(recipe["packages"]), {"sample-cli", "sample-gui"})

    def test_empty_override_and_arch_specific_fields(self):
        recipe = arch.srcinfo("pkgbase = sample\narch = any\ndepends = common\ndepends_x86_64 = arch-lib\npkgname = sample\ndepends = \n", "x86_64")
        self.assertEqual(recipe["runtime"], ["arch-lib"])
        with self.assertRaises(arch.PreparationError):
            arch.srcinfo(RECIPE, "aarch64")

    def test_invalid_names_and_dependencies(self):
        for value in ["-argument", "../package", "name;command", "pkg >=1"]:
            with self.assertRaises(arch.PreparationError):
                arch.spec(value)
        self.assertEqual(arch.spec("library>=2"), ("library", ">=", "2"))

    def test_official_resolution_uses_empty_local_database(self):
        p = arch.Preparer.__new__(arch.Preparer)
        p.pacman = Mock(return_value=subprocess.CompletedProcess([], 0, "pkg.pkg.tar.zst\ndep.pkg.tar.zst\n", ""))
        self.assertEqual(p.official("pkg"), ["pkg.pkg.tar.zst", "dep.pkg.tar.zst"])
        self.assertTrue(p.pacman.call_args.kwargs["isolated"])

    def test_database_errors_do_not_become_aur_requests(self):
        p = arch.Preparer.__new__(arch.Preparer)
        p.pacman = Mock(return_value=subprocess.CompletedProcess([], 1, "", "database is corrupted"))
        with self.assertRaises(arch.PreparationError):
            p.official("pkg")
        p.pacman.return_value.stderr = "error: target not found: pkg"
        self.assertIsNone(p.official("pkg"))

    def test_recursive_official_closure_requires_all_files(self):
        with tempfile.TemporaryDirectory() as temporary:
            p = arch.Preparer.__new__(arch.Preparer)
            p.cache = Path(temporary)
            p.pacman = Mock()
            p.fetched, p.files = set(), set()
            (p.cache / "pkg").write_text("package")
            with self.assertRaises(arch.PreparationError):
                p.fetch("pkg", ["pkg", "dependency"])
            (p.cache / "dependency").write_text("dependency")
            p.fetch("pkg", ["pkg", "dependency"])
            self.assertEqual(p.files, {"pkg", "dependency"})
            self.assertEqual(p.pacman.call_count, 1)

    def test_build_cycle_is_explicit(self):
        p = arch.Preparer.__new__(arch.Preparer)
        p.states, p.stack = {"sample": "building"}, ["sample"]
        with self.assertRaisesRegex(arch.PreparationError, "dependency cycle"):
            p.build({"name": "sample"})
        p.states["sample"] = "built"
        p.build({"name": "sample"})

    def test_explicit_provider_disambiguates_virtual_name(self):
        p = arch.Preparer.__new__(arch.Preparer)
        candidate = dict(name="chosen", provides={"chosen": ["virtual=2"]})
        p.bases = {"chosen": candidate}
        p.rpc = Mock(return_value=[])
        p.load_base = Mock(return_value=candidate)
        self.assertEqual(p.aur("virtual"), candidate)
        p.rpc.assert_called_once_with("info", "virtual")

    def test_virtual_provider_search_filters_nonmatching_results(self):
        p = arch.Preparer.__new__(arch.Preparer)
        p.bases = {}
        p.rpc = Mock(side_effect=[[], [{"PackageBase": "wrong"}, {"PackageBase": "right"}]])
        p.load_base = lambda n: dict(name=n, provides={n: ["other"] if n == "wrong" else ["virtual"]})
        self.assertEqual(p.aur("virtual")["name"], "right")

    def test_every_recipe_command_uses_unprivileged_account(self):
        p = arch.Preparer.__new__(arch.Preparer)
        p.builder = Mock(pw_dir="/builder-home")
        p.keys = Path("/keys")
        with patch.object(arch, "run") as run:
            p.user(["makepkg", "--force"])
            command = run.call_args.args[0]
            self.assertEqual(command[:5], ["runuser", "-u", "rstar-build", "--", "env"])
            self.assertIn("GNUPGHOME=/keys", command)

    def test_atomic_copy_replaces_symlink_without_touching_target(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "outside").write_text("original")
            (root / "source").write_text("new")
            (root / "destination").symlink_to("outside")
            arch.atomic_copy(root / "source", root / "destination")
            self.assertEqual((root / "outside").read_text(), "original")
            self.assertEqual((root / "destination").read_text(), "new")
            self.assertFalse((root / "destination").is_symlink())

    def test_build_discovers_dynamic_outputs_and_prepares_binary_runtime_dependencies(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            p = arch.Preparer.__new__(arch.Preparer)
            p.states, p.stack, p.artifacts = {}, [], {}
            p.builder = Mock(pw_uid=os.getuid(), pw_gid=os.getgid())
            p.outputs, p.repo, p.cache = root / "outputs", root / "repo", root / "cache"
            for directory in (p.outputs, p.repo, p.cache):
                directory.mkdir()
            p.makepkg_conf = root / "makepkg.conf"
            p.makepkg_conf.write_text("# configuration\n")
            p.ensure, p.publish = Mock(), Mock()
            calls = []

            def makepkg(checkout, arguments):
                calls.append(arguments)
                if "--force" in arguments:
                    for name in ("sample-cli", "sample-gui", "sample-cli-debug"):
                        (p.outputs / "sample" / f"{name}-dynamic.pkg.tar.zst").write_text("binary")

            def inspect(command, **kwargs):
                package = Path(command[2]).name.split("-dynamic")[0]
                return subprocess.CompletedProcess(command, 0,
                    f"pkgname = {package}\npkgver = 2.r15-1\ndepend = generated-runtime\n", "")

            p.makepkg = makepkg
            base = arch.srcinfo(RECIPE, "x86_64")
            base.update(checkout=root, commit="current-recipe")
            with patch.object(arch, "run", side_effect=inspect):
                p.build(base)
            self.assertEqual(len(p.artifacts["sample"]), 3)
            self.assertEqual(p.states["sample"], "complete")
            self.assertIn(["--force", "--cleanbuild", "--clean", "--nodeps", "--noconfirm"], calls)
            self.assertFalse(any("--packagelist" in call for call in calls))
            p.ensure.assert_any_call("compiler", install=True)
            p.ensure.assert_any_call("common", install=True)
            p.ensure.assert_any_call("generated-runtime")
            self.assertFalse(any(call.args == ("gui-library",) and call.kwargs.get("install") for call in p.ensure.call_args_list))

    def test_manifest_is_not_written_for_corrupt_official_package(self):
        with tempfile.TemporaryDirectory() as temporary:
            p = arch.Preparer.__new__(arch.Preparer)
            p.cache = p.repo = p.work = Path(temporary)
            p.files, p.artifacts = {"pkg"}, {}
            (p.cache / "pkg").write_text("corrupted")
            p.official_metadata = {"pkg": {"SHA256SUM": ["0" * 64]}}
            with self.assertRaisesRegex(arch.PreparationError, "checksum mismatch"):
                p.finish(["pkg"])
            self.assertFalse((p.repo / ".prepared.json").exists())


if __name__ == "__main__":
    unittest.main()

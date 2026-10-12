"""Protocol tests and optional offline pacman checks in disposable databases."""
import hashlib
import importlib.util
import os
import shutil
import subprocess
import tarfile
import tempfile
import unittest
from io import BytesIO
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
    def preparer(self, cache, packages):
        p = arch.Preparer.__new__(arch.Preparer)
        p.cache = Path(cache)
        p.files, p.resolutions = set(), {}
        p.official_metadata = {
            filename: {"NAME": [name], "VERSION": [version]}
            for filename, name, version in packages
        }
        return p

    def test_official_batch_downloads_shared_dependencies_once_and_caches_resolution(self):
        with tempfile.TemporaryDirectory() as temporary:
            p = self.preparer(temporary, [(name, name, "1-1") for name in ("desktop", "server", "shared")])
            closures = {"desktop": ["shared", "desktop"], "server": ["shared", "server"]}
            downloads = []

            def pacman(arguments, **kwargs):
                self.assertTrue(kwargs["isolated"])
                if arguments[0] == "-Sp":
                    requirement = arguments[-1]
                    if requirement not in closures:
                        return subprocess.CompletedProcess([], 1, "", f"error: target not found: {requirement}")
                    return subprocess.CompletedProcess([], 0, "\n".join(closures[requirement]) + "\n", "")
                downloads.append(arguments)
                for target in arguments[arguments.index("--") + 1:]:
                    (p.cache / target.split("=", 1)[0]).write_text("package")
                return subprocess.CompletedProcess([], 0, "", "")

            p.pacman = Mock(side_effect=pacman)
            self.assertEqual(p.prefetch(["desktop", "server", "desktop", "aur-tool"]), ["aur-tool"])
            self.assertEqual(p.files, {"desktop", "server", "shared"})
            self.assertEqual(len(downloads), 1)
            self.assertEqual(downloads[0][downloads[0].index("--") + 1:],
                             ["desktop=1-1", "server=1-1", "shared=1-1"])
            p.ensure("desktop")
            p.prefetch(["server", "aur-tool"])
            self.assertEqual(len(downloads), 1)
            self.assertEqual(sum(call.args[0][0] == "-Sp" for call in p.pacman.call_args_list), 3)

    def test_batch_keeps_each_requirements_chosen_virtual_provider(self):
        with tempfile.TemporaryDirectory() as temporary:
            p = self.preparer(temporary, [(name, name, "1-1") for name in ("provider-one", "provider-two")])

            def pacman(arguments, **kwargs):
                if arguments[0] == "-Sp":
                    provider = "provider-one" if arguments[-1] == "virtual" else "provider-two"
                    return subprocess.CompletedProcess([], 0, provider + "\n", "")
                for target in arguments[arguments.index("--") + 1:]:
                    (p.cache / target.split("=", 1)[0]).write_text("package")
                return subprocess.CompletedProcess([], 0, "", "")

            p.pacman = Mock(side_effect=pacman)
            p.prefetch(["virtual", "provider-two"])
            self.assertEqual(p.files, {"provider-one", "provider-two"})
            download = p.pacman.call_args.args[0]
            self.assertEqual(download[download.index("--") + 1:], ["provider-one=1-1", "provider-two=1-1"])

    def test_batch_separates_two_versions_of_the_same_package(self):
        with tempfile.TemporaryDirectory() as temporary:
            p = self.preparer(temporary, [("library-1", "library", "1-1"), ("library-2", "library", "2-1")])

            def pacman(arguments, **kwargs):
                targets = arguments[arguments.index("--") + 1:]
                self.assertEqual(len(targets), 1)
                version = targets[0].split("=", 1)[1].split("-", 1)[0]
                (p.cache / f"library-{version}").write_text("package")
                return subprocess.CompletedProcess([], 0, "", "")

            p.pacman = Mock(side_effect=pacman)
            p.fetch(["library-1", "library-2"])
            self.assertEqual(p.pacman.call_count, 2)
            self.assertEqual(p.files, {"library-1", "library-2"})

    def test_cached_but_unverified_files_still_get_a_download_verification(self):
        with tempfile.TemporaryDirectory() as temporary:
            p = self.preparer(temporary, [("pkg", "pkg", "1-1")])
            (p.cache / "pkg").write_text("existing package")
            p.pacman = Mock(side_effect=arch.PreparationError("invalid package signature"))
            with self.assertRaisesRegex(arch.PreparationError, "invalid package signature"):
                p.fetch(["pkg"])
            self.assertFalse(p.files)

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
        p.resolutions = {}
        p.pacman = Mock(return_value=subprocess.CompletedProcess([], 0, "pkg.pkg.tar.zst\ndep.pkg.tar.zst\n", ""))
        self.assertEqual(p.official("pkg"), ["pkg.pkg.tar.zst", "dep.pkg.tar.zst"])
        self.assertTrue(p.pacman.call_args.kwargs["isolated"])

    def test_official_group_downloads_all_members_without_aur_lookup(self):
        with tempfile.TemporaryDirectory() as temporary:
            filenames = ["gnome-shell.pkg.tar.zst", "nautilus.pkg.tar.zst"]
            p = self.preparer(temporary, [(filename, filename.split(".", 1)[0], "1-1") for filename in filenames])
            for name in filenames:
                (p.cache / name).write_text("package")
            p.pacman = Mock(side_effect=[
                subprocess.CompletedProcess([], 0, "\n".join(filenames) + "\n", ""),
                subprocess.CompletedProcess([], 0, "", ""),
            ])
            p.aur = Mock(side_effect=AssertionError("official group treated as AUR"))
            p.ensure("gnome")
            self.assertEqual(p.files, set(filenames))
            self.assertEqual(p.resolutions, {"gnome": filenames})
            self.assertEqual(p.pacman.call_args_list[0].args[0],
                             ["-Sp", "--noconfirm", "--print-format", "%f", "--", "gnome"])
            self.assertEqual(p.pacman.call_args_list[1].args[0],
                             ["-Sw", "--nodeps", "--nodeps", "--noconfirm", "--", "gnome-shell=1-1", "nautilus=1-1"])
            self.assertTrue(all(call.kwargs["isolated"] for call in p.pacman.call_args_list))
            p.aur.assert_not_called()

    def test_database_errors_do_not_become_aur_requests(self):
        p = arch.Preparer.__new__(arch.Preparer)
        p.resolutions = {}
        p.pacman = Mock(return_value=subprocess.CompletedProcess([], 1, "", "database is corrupted"))
        with self.assertRaises(arch.PreparationError):
            p.official("pkg")
        p.pacman.return_value.stderr = "error: target not found: pkg"
        self.assertIsNone(p.official("pkg"))

    def test_recursive_official_closure_requires_all_files(self):
        with tempfile.TemporaryDirectory() as temporary:
            p = self.preparer(temporary, [(name, name, "1-1") for name in ("pkg", "dependency")])
            p.pacman = Mock()
            (p.cache / "pkg").write_text("package")
            with self.assertRaises(arch.PreparationError):
                p.fetch(["pkg", "dependency"])
            self.assertFalse(p.files)
            (p.cache / "dependency").write_text("dependency")
            p.fetch(["pkg", "dependency"])
            p.fetch(["pkg", "dependency"])
            self.assertEqual(p.files, {"pkg", "dependency"})
            self.assertEqual(p.pacman.call_count, 2)

    def test_main_batches_base_and_official_targets_before_building_aur(self):
        with tempfile.TemporaryDirectory() as temporary:
            p = self.preparer(temporary, [(name, name, "1-1") for name in ("base", "desktop", "shared")])
            downloads = []

            def pacman(arguments, **kwargs):
                if arguments[0] == "-Sp":
                    requirement = arguments[-1]
                    if requirement == "aur-tool":
                        return subprocess.CompletedProcess([], 1, "", "error: target not found: aur-tool")
                    return subprocess.CompletedProcess([], 0, f"{requirement}\nshared\n", "")
                downloads.append(arguments)
                for target in arguments[arguments.index("--") + 1:]:
                    (p.cache / target.split("=", 1)[0]).write_text("package")
                return subprocess.CompletedProcess([], 0, "", "")

            p.pacman = Mock(side_effect=pacman)
            p.initialize, p.load_base, p.aur, p.ensure, p.clean_dependencies, p.finish = [Mock() for _ in range(6)]
            p.rpc = Mock(return_value=[{"PackageBase": "aur-tool"}])
            p.ensure.side_effect = lambda requirement: self.assertEqual(p.files, {"base", "desktop", "shared"})
            with patch.object(arch, "Preparer", return_value=p), patch.object(arch.sys, "argv", ["predownload.py", "aur-tool", "desktop", "desktop"]):
                arch.main()
            self.assertEqual(len(downloads), 1)
            p.ensure.assert_called_once_with("aur-tool")
            p.clean_dependencies.assert_called_once_with()
            p.finish.assert_called_once_with(["base", "aur-tool", "desktop"])

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

    def test_published_package_is_readable_with_private_source_and_restrictive_umask(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source, destination = root / "built.pkg.tar.zst", root / "published.pkg.tar.zst"
            source.write_bytes(b"new package")
            destination.write_bytes(b"old package")
            for path in (source, destination):
                path.chmod(0o600)
            previous_umask = os.umask(0o077)
            try:
                arch.atomic_copy(source, destination)
            finally:
                os.umask(previous_umask)
            self.assertEqual(destination.read_bytes(), b"new package")
            self.assertEqual(destination.stat().st_mode & 0o777, 0o644)
            self.assertEqual(source.stat().st_mode & 0o777, 0o600)
            self.assertEqual(set(root.iterdir()), {source, destination})

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
            p.ensure, p.publish, p.prefetch = Mock(), Mock(), Mock()
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
            self.assertEqual([call.args[0] for call in p.prefetch.call_args_list],
                             [["compiler", "common"], ["generated-runtime"]])
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


class OfflinePacman(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        pacman = shutil.which("pacman")
        if not pacman:
            raise unittest.SkipTest("pacman is unavailable")
        cls.command = [pacman]
        if os.geteuid() != 0:
            unshare = shutil.which("unshare")
            if not unshare:
                raise unittest.SkipTest("user namespaces are unavailable")
            namespace = [unshare, "--user", "--map-root-user"]
            if subprocess.run([*namespace, "true"], capture_output=True).returncode:
                raise unittest.SkipTest("unprivileged user namespaces are disabled")
            cls.command = [*namespace, pacman]

    def test_conflicting_packages_providers_and_versions_are_cached_without_installing(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            db, cache = root / "db", root / "cache"
            for path in (db / "local", db / "sync", cache):
                path.mkdir(parents=True)
            p = arch.Preparer.__new__(arch.Preparer)
            p.cache, p.files, p.resolutions, p.official_metadata = cache, set(), {}, {}
            config = "[options]\nArchitecture = x86_64\nSigLevel = Never\n"
            repositories = {
                "primary": [
                    ("shared", "1-1", {}),
                    ("alpha", "1-1", {"DEPENDS": ["shared"], "CONFLICTS": ["beta"], "GROUPS": ["desktop"]}),
                    ("beta", "1-1", {"DEPENDS": ["shared"], "CONFLICTS": ["alpha"]}),
                    ("provider-one", "1-1", {"PROVIDES": ["virtual=1"]}),
                    ("provider-two", "1-1", {"PROVIDES": ["virtual=2"]}),
                    ("consumer", "1-1", {"DEPENDS": ["virtual"], "GROUPS": ["desktop"]}),
                    ("library", "2-1", {}),
                    ("old-client", "1-1", {"DEPENDS": ["library<2"]}),
                    ("new-client", "1-1", {"DEPENDS": ["library>=2"]}),
                ],
                "secondary": [("library", "1-1", {})],
            }
            for repository, packages in repositories.items():
                source = root / repository
                source.mkdir()
                config += f"[{repository}]\nServer = {source.as_uri()}\n"
                with tarfile.open(db / "sync" / f"{repository}.db", "w:gz") as database:
                    for name, version, extra in packages:
                        filename = f"{name}-{version}-x86_64.pkg.tar.gz"
                        package = source / filename
                        with tarfile.open(package, "w:gz") as archive:
                            data = f"pkgname = {name}\npkgver = {version}\npkgdesc = fixture\narch = x86_64\n".encode()
                            member = tarfile.TarInfo(".PKGINFO")
                            member.size = len(data)
                            archive.addfile(member, BytesIO(data))
                        fields = {"NAME": [name], "VERSION": [version], "FILENAME": [filename],
                                  "CSIZE": [str(package.stat().st_size)], "ISIZE": ["0"],
                                  "SHA256SUM": [hashlib.sha256(package.read_bytes()).hexdigest()], **extra}
                        p.official_metadata[filename] = fields
                        data = "".join("%" + key + "%\n" + "\n".join(values) + "\n\n" for key, values in fields.items()).encode()
                        member = tarfile.TarInfo(f"{name}-{version}/desc")
                        member.size = len(data)
                        database.addfile(member, BytesIO(data))
            configuration = root / "pacman.conf"
            configuration.write_text(config)
            command = [*self.command, "--config", str(configuration), "--dbpath", str(db),
                       "--cachedir", str(cache), "--logfile", str(root / "pacman.log")]
            downloads = []

            def pacman(arguments, **kwargs):
                if arguments[0] == "-Sw":
                    downloads.append(arguments)
                result = subprocess.run([*command, *arguments], text=True, capture_output=True)
                self.assertIn(result.returncode, kwargs.get("allowed", (0,)), result.stderr)
                return result

            p.pacman = pacman
            targets = ["desktop", "beta", "virtual", "provider-two"]
            self.assertEqual(p.prefetch(targets), [])
            self.assertEqual(len(downloads), 1)
            self.assertEqual(p.files, {f"{name}-1-1-x86_64.pkg.tar.gz" for name in
                                      ("alpha", "beta", "consumer", "provider-one", "provider-two", "shared")})
            p.prefetch(targets)
            self.assertEqual(len(downloads), 1)
            p.prefetch(["old-client", "new-client"])
            self.assertEqual(len(downloads), 3)
            self.assertTrue({"library-1-1-x86_64.pkg.tar.gz", "library-2-1-x86_64.pkg.tar.gz"} <= p.files)
            p.prefetch(["library<2", "library>=2"])
            self.assertEqual(len(downloads), 3)
            self.assertTrue(all((cache / name).is_file() for name in p.files))
            self.assertFalse(any(path.is_dir() for path in (db / "local").iterdir()))


if __name__ == "__main__":
    unittest.main()

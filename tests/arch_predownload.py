"""Protocol tests and optional offline pacman checks in disposable databases."""
import hashlib
import importlib.util
import json
import os
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest
from io import BytesIO
from io import StringIO
from contextlib import redirect_stderr, redirect_stdout
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

    def plan_preparer(self, packages):
        temporary = tempfile.TemporaryDirectory(prefix="rstarsleep-offline-plan-")
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        p = arch.Preparer.__new__(arch.Preparer)
        p.work, p.db, p.resolutions = root, root / "db", {}
        for directory in (p.db / "local", p.db / "sync", root / "cache"):
            directory.mkdir(parents=True)
        p.configuration = "[options]\nArchitecture = x86_64\nSigLevel = Never\n[fixture]\nServer = file:///nonexistent\n"
        p.official_conf = root / "pacman.conf"
        p.official_conf.write_text(p.configuration)
        p.official_metadata = {}
        with tarfile.open(p.db / "sync/fixture.db", "w:gz") as database:
            for name, version, extra in packages:
                filename = f"{name}-{version}-x86_64.pkg.tar.zst"
                fields = {"NAME": [name], "VERSION": [version], "FILENAME": [filename],
                          "CSIZE": ["1"], "ISIZE": ["1"], **extra}
                p.official_metadata[filename] = fields
                data = "".join("%" + key + "%\n" + "\n".join(values) + "\n\n" for key, values in fields.items()).encode()
                member = tarfile.TarInfo(f"{name}-{version}/desc")
                member.size = len(data)
                database.addfile(member, BytesIO(data))

        def pacman(arguments, **kwargs):
            self.assertEqual(arguments[0], "-Sp", "collector must only query metadata")
            self.assertTrue(kwargs["isolated"])
            result = subprocess.run([*self.command, "--config", str(p.official_conf), "--dbpath", str(p.db),
                                     "--cachedir", str(root / "cache"), "--logfile", "/dev/null", *arguments],
                                    text=True, capture_output=True)
            self.assertIn(result.returncode, kwargs.get("allowed", (0,)), result.stderr)
            return result

        p.pacman = Mock(side_effect=pacman)
        p.fetch = Mock(side_effect=AssertionError("collector downloaded packages"))
        p.build = Mock(side_effect=AssertionError("collector compiled packages"))
        return p

    def test_plan_uses_explicit_official_provider_in_the_combined_transaction(self):
        p = self.plan_preparer([
            ("btrfs-progs", "1-1", {"PROVIDES": ["virtual-tool=1"]}),
            ("z-alternative", "1-1", {"PROVIDES": ["virtual-tool=1"]}),
            ("consumer", "1-1", {"DEPENDS": ["virtual-tool"]}),
        ])
        self.assertEqual(p.collect(["consumer", "z-alternative"]), ["consumer", "z-alternative"])
        self.assertEqual(p.pacman.call_args.args[0][-2:], ["consumer", "z-alternative"])
        p.fetch.assert_not_called()
        p.build.assert_not_called()

    def test_plan_uses_explicit_aur_provider_for_official_runtime_dependencies(self):
        p = self.plan_preparer([
            ("btrfs-progs", "1-1", {"PROVIDES": ["virtual-tool=1"]}),
            ("consumer", "1-1", {"DEPENDS": ["virtual-tool"]}),
        ])
        recipe = arch.srcinfo("pkgbase = z-alternative\npkgver = 1\npkgrel = 1\npkgname = z-alternative\nprovides = virtual-tool=1\nconflicts = btrfs-progs\n", "x86_64")
        p.aur = Mock(return_value=recipe)
        original_configuration = p.official_conf
        self.assertEqual(p.collect(["consumer", "z-alternative"]), ["consumer", "z-alternative"])
        self.assertEqual(p.official_conf, original_configuration)
        self.assertEqual(list((p.db / "sync").iterdir()), [p.db / "sync/fixture.db"])

    def test_plan_uses_explicit_official_provider_for_aur_runtime_dependencies(self):
        p = self.plan_preparer([
            ("btrfs-progs", "1-1", {"PROVIDES": ["virtual-tool=1"]}),
            ("z-alternative", "1-1", {"PROVIDES": ["virtual-tool=1"]}),
        ])
        recipe = arch.srcinfo("pkgbase = consumer\npkgver = 1\npkgrel = 1\npkgname = consumer\ndepends = virtual-tool\n", "x86_64")
        p.aur = Mock(return_value=recipe)
        self.assertEqual(p.collect(["consumer", "z-alternative"]), ["consumer", "z-alternative"])

    def test_plan_restores_official_configuration_when_joint_resolution_fails(self):
        p = self.plan_preparer([])
        recipe = arch.srcinfo("pkgbase = consumer\npkgver = 1\npkgrel = 1\npkgname = consumer\ndepends = absent\n", "x86_64")
        p.aur = Mock(return_value=recipe)
        # Select a recipe whose dependency becomes unsatisfied after discovery.
        p.official = Mock(side_effect=[None, ["changed.pkg"]])
        original_configuration = p.official_conf
        with self.assertRaisesRegex(arch.PreparationError, "cannot be resolved"):
            p.collect(["consumer"])
        self.assertEqual(p.official_conf, original_configuration)
        self.assertEqual(list((p.db / "sync").iterdir()), [p.db / "sync/fixture.db"])

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


class PackagePlans(unittest.TestCase):
    def test_query_keeps_child_diagnostics_out_of_json_stdout(self):
        preparer = Mock()
        preparer.initialize.side_effect = lambda **kwargs: arch.run([
            sys.executable, "-c", "print('repository refreshed')"])
        preparer.collect.return_value = ["base", "coreutils"]
        output = StringIO()
        with tempfile.TemporaryFile(mode="w+") as diagnostics:
            with patch.object(arch, "Preparer", return_value=preparer), \
                    redirect_stdout(output), redirect_stderr(diagnostics):
                arch.collect_main(["base", "base"])
            diagnostics.seek(0)
            self.assertEqual(diagnostics.read(), "repository refreshed\n")
        self.assertEqual(json.loads(output.getvalue()),
                         {"schema_version": 1, "packages": ["base", "coreutils"]})
        preparer.initialize.assert_called_once_with(planning=True)
        preparer.collect.assert_called_once_with(["base"])
        self.assertFalse(arch.QUERY_MODE)

    def test_failed_query_restores_output_mode(self):
        with self.assertRaises(arch.PreparationError):
            arch.collect_main(["../invalid"])
        self.assertFalse(arch.QUERY_MODE)

    def preparer(self, closures):
        preparer = arch.Preparer.__new__(arch.Preparer)
        preparer.resolutions = {}
        preparer.official_metadata = {
            name + ".pkg": {"NAME": [name], "VERSION": ["1-1"]}
            for names in closures.values() for name in names
        }
        def pacman(arguments, **kwargs):
            self.assertEqual(arguments[0], "-Sp", "collector must not install or download")
            self.assertTrue(kwargs["isolated"])
            target = arguments[-1]
            if target not in closures:
                return subprocess.CompletedProcess([], 1, "", f"error: target not found: {target}")
            return subprocess.CompletedProcess([], 0, "\n".join(name + ".pkg" for name in closures[target]) + "\n", "")
        preparer.pacman = Mock(side_effect=pacman)
        def plan_transaction(requested, selected):
            filenames, metadata = set(), dict(preparer.official_metadata)
            for requirement in [*requested, *(dependency for artifact in selected.values() for dependency in artifact["dependencies"])]:
                if requirement in closures:
                    filenames.update(name + ".pkg" for name in closures[requirement])
            for name in selected:
                filename = name + ".aur.pkg"
                filenames.add(filename)
                metadata[filename] = {"NAME": [name]}
            return sorted(filenames), metadata
        preparer._plan_transaction = Mock(side_effect=plan_transaction)
        preparer.fetch = Mock(side_effect=AssertionError("collector downloaded packages"))
        preparer.build = Mock(side_effect=AssertionError("collector compiled an AUR package"))
        return preparer

    def test_group_base_and_runtime_closures_are_collected_without_package_downloads(self):
        preparer = self.preparer({"base": ["base", "coreutils"], "desktop": ["linux", "mkinitcpio", "libdependency"]})
        self.assertEqual(preparer.collect(["base", "desktop"]), ["base", "coreutils", "libdependency", "linux", "mkinitcpio"])
        self.assertNotIn("base", preparer.collect(["desktop"]))  # Core explicitly supplies declared base roots.
        preparer.fetch.assert_not_called()
        preparer.build.assert_not_called()

    def test_aur_selects_only_requested_split_package_and_its_runtime_dependencies(self):
        preparer = self.preparer({"common": ["common"]})
        recipe = arch.srcinfo(RECIPE, "x86_64")
        preparer.aur = Mock(return_value=recipe)
        self.assertEqual(preparer.collect(["sample-cli"]), ["common", "sample-cli"])
        preparer.aur.assert_called_once_with("sample-cli")
        self.assertNotIn("compiler", preparer.collect(["sample-cli"]))
        self.assertNotIn("gui-library", preparer.collect(["sample-cli"]))
        self.assertNotIn("sample-gui", preparer.collect(["sample-cli"]))

    def test_selected_aur_provider_satisfies_runtime_dependency_without_adding_another_package(self):
        preparer = self.preparer({})
        recipe = arch.srcinfo(RECIPE, "x86_64")
        consumer = arch.srcinfo("pkgbase = consumer\npkgver = 1\npkgrel = 1\npkgname = consumer\ndepends = virtual-gui=1\n", "x86_64")
        recipe["runtime_by_package"]["sample-gui"] = []
        preparer.aur = Mock(side_effect=lambda target: consumer if target == "consumer" else recipe)
        self.assertEqual(preparer.collect(["consumer", "sample-gui"]), ["consumer", "sample-gui"])
        self.assertEqual(preparer.aur.call_count, 2)

    def test_aur_cycles_terminate_and_transitive_runtime_dependencies_are_included(self):
        preparer = self.preparer({"library": ["library", "libsupport"]})
        recipes = {name: arch.srcinfo(
            f"pkgbase = {name}\npkgver = 1\npkgrel = 1\npkgname = {name}\ndepends = {dependency}\n", "x86_64")
            for name, dependency in [("one", "two"), ("two", "one")]}
        recipes["two"]["runtime_by_package"]["two"].append("library")
        preparer.aur = Mock(side_effect=lambda target: recipes[target])
        self.assertEqual(preparer.collect(["one"]), ["library", "libsupport", "one", "two"])

    def test_invalid_official_metadata_and_ambiguous_aur_providers_fail_closed(self):
        preparer = self.preparer({"base": ["base"]})
        preparer.official_metadata.clear()
        with self.assertRaisesRegex(arch.PreparationError, "identity"):
            preparer.collect(["base"])
        preparer = self.preparer({})
        recipe = arch.srcinfo(RECIPE, "x86_64")
        preparer.aur = Mock(return_value=recipe)
        with self.assertRaisesRegex(arch.PreparationError, "uniquely satisfy"):
            preparer.collect(["unprovided"])

    def test_planner_refreshes_metadata_without_updating_or_installing_the_resolver_environment(self):
        with tempfile.TemporaryDirectory() as temporary:
            preparer = arch.Preparer.__new__(arch.Preparer)
            preparer.db = Path(temporary) / "db"
            (preparer.db / "sync").mkdir(parents=True)
            preparer.actual_db = Path(temporary) / "actual"
            preparer.pacman = Mock(return_value=subprocess.CompletedProcess([], 0, "", ""))
            preparer.refresh_repo = Mock()
            preparer.initialize(planning=True)
            self.assertEqual(preparer.pacman.call_count, 1)
            self.assertEqual(preparer.pacman.call_args.args[0], ["-Sy", "--noconfirm"])


if __name__ == "__main__":
    unittest.main()

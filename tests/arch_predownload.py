"""Protocol tests and optional offline pacman checks in disposable databases."""
import hashlib
import importlib.util
import json
import os
import shutil
import socket
import subprocess
import sys
import tarfile
import tempfile
import threading
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


class ChecksumProgress(unittest.TestCase):
    class Terminal(StringIO):
        def isatty(self):
            return True

    def test_consecutive_updates_use_one_terminal_row(self):
        output = self.Terminal()
        with patch.dict(os.environ, {"TERM": "xterm"}), redirect_stderr(output):
            with arch.checksum_progress() as progress:
                progress.update(25, 520)
                progress.update(50, 520)
                progress.update(520, 520)
        self.assertEqual(output.getvalue().count("\n"), 1)
        self.assertEqual(output.getvalue().count("\r\x1b[2K"), 2)
        self.assertTrue(output.getvalue().endswith("520/520\n"))

    def test_intervening_output_is_preserved_and_starts_a_new_progress_row(self):
        for stream in ("stderr", "stdout"):
            with self.subTest(stream=stream):
                diagnostics, protocol = self.Terminal(), StringIO()
                with patch.dict(os.environ, {"TERM": "xterm"}), \
                        redirect_stderr(diagnostics), redirect_stdout(protocol):
                    with arch.checksum_progress() as progress:
                        progress.update(25, 520)
                        print("intervening output", file=getattr(sys, stream))
                        progress.update(50, 520)
                self.assertNotIn("\r", diagnostics.getvalue())
                self.assertIn("25/520\n", diagnostics.getvalue())
                self.assertTrue(diagnostics.getvalue().endswith("50/520\n"))
                if stream == "stdout":
                    self.assertEqual(protocol.getvalue(), "intervening output\n")
                else:
                    self.assertIn("25/520\nintervening output\n", diagnostics.getvalue())

    def test_redirected_and_dumb_terminals_keep_plain_logs(self):
        for output, term in ((StringIO(), "xterm"), (self.Terminal(), "dumb")):
            with self.subTest(term=term), patch.dict(os.environ, {"TERM": term}), \
                    redirect_stderr(output):
                with arch.checksum_progress() as progress:
                    progress.update(25, 520)
                    progress.update(520, 520)
                self.assertNotIn("\r", output.getvalue())
                self.assertNotIn("\x1b", output.getvalue())
                self.assertEqual(output.getvalue().count("Final verification progress:"), 2)

    def test_failure_ends_progress_and_restores_streams(self):
        output, protocol = self.Terminal(), StringIO()
        with patch.dict(os.environ, {"TERM": "xterm"}), \
                redirect_stderr(output), redirect_stdout(protocol):
            with self.assertRaises(arch.PreparationError):
                with arch.checksum_progress() as progress:
                    progress.update(25, 520)
                    raise arch.PreparationError("checksum mismatch")
            self.assertIs(sys.stderr, output)
            self.assertIs(sys.stdout, protocol)
            arch.log("校验失败", "checksum mismatch")
        self.assertIn("25/520\n==> [RStarSleep]", output.getvalue())
        self.assertIn("checksum mismatch", output.getvalue())

    def test_narrow_terminal_does_not_wrap(self):
        output = self.Terminal()
        with patch.dict(os.environ, {"TERM": "xterm"}), redirect_stderr(output), \
                patch.object(arch.os, "get_terminal_size", return_value=os.terminal_size((6, 24))):
            # StringIO has no descriptor; provide one for the mocked size query.
            with patch.object(output, "fileno", return_value=2):
                with arch.checksum_progress() as progress:
                    progress.update(25, 520)
        self.assertEqual(output.getvalue(), "4%\n")


class SourceTargets(unittest.TestCase):
    def test_target_validation(self):
        for value in ("paru", "archlinuxcn/paru", "aur/paru"):
            self.assertTrue(arch.valid_target(value))
        for value in ("/paru", "aur/", "aur/x/y", "--repo/paru", "aur/bad name"):
            self.assertFalse(arch.valid_target(value))

    def test_forced_aur_skips_repository(self):
        preparer = object.__new__(arch.Preparer)
        preparer.pacman = Mock(side_effect=AssertionError("repository queried"))
        self.assertIsNone(preparer.official("aur/paru"))
        self.assertFalse(preparer.official_target("aur/paru"))
        self.assertEqual(arch.spec("aur/paru"), ("paru", None, None))

    def test_selected_repository_does_not_fall_back(self):
        preparer = object.__new__(arch.Preparer)
        preparer.resolutions = {}
        preparer.pacman = Mock(return_value=Mock(returncode=1, stderr="target not found: paru"))
        with self.assertRaises(arch.PreparationError):
            preparer.official("archlinuxcn/paru")
        self.assertEqual(preparer.pacman.call_args.args[0][-1], "archlinuxcn/paru")


class Configuration(unittest.TestCase):
    def fixture(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        plugin = root / "plugin with spaces & $literal"
        for relative in ("lib", "data", "state/packages", "state/repository", "state/sources"):
            (plugin / relative).mkdir(parents=True, exist_ok=True)
        shutil.copyfile(module_path.with_name("common.sh"), plugin / "lib/common.sh")
        shutil.copyfile(module_path.with_name("keyrings.sh"), plugin / "lib/keyrings.sh")
        (plugin / "data/pacman.conf.in").write_text(
            "[options]\nArchitecture = auto\nSigLevel = Required DatabaseOptional\n"
            "[custom]\nInclude = @MIRRORLIST@\n"
            "[rstarsleep]\nServer = file:///stale\n"
            "[fixture]\nServer = file://@PLUGIN_ROOT@/fixtures/$repo\n")
        (plugin / "data/mirrorlist").write_text("Server = https://default.invalid/$repo/os/$arch\n")
        workspace = root / "workspace"
        workspace.mkdir()
        return plugin, workspace

    def initialize(self, plugin, workspace, environment=None):
        original_read_text = Path.read_text

        def read_text(path, *args, **kwargs):
            if path == Path("/etc/pacman.conf"):
                raise AssertionError("resolver must not read the system pacman configuration")
            if path == Path("/etc/makepkg.conf"):
                return "# isolated makepkg configuration\n"
            return original_read_text(path, *args, **kwargs)

        variables = {"RSTAR_PLUGIN_ROOT": str(plugin), "RSTAR_DISTRIBUTION_MIRROR": "", "RSTAR_MIRROR": ""}
        variables.update(environment or {})
        with patch.dict(os.environ, variables), \
                patch.object(arch.os, "geteuid", return_value=0), \
                patch.object(arch.os, "chown"), \
                patch.object(arch.pwd, "getpwnam", return_value=Mock(pw_uid=12345, pw_gid=12345)), \
                patch.object(Path, "read_text", read_text):
            return arch.Preparer(workspace)

    def test_preparer_uses_plugin_repositories_and_readable_runtime_configuration(self):
        plugin, workspace = self.fixture()
        previous_umask = os.umask(0o077)
        try:
            preparer = self.initialize(plugin, workspace)
        finally:
            os.umask(previous_umask)
        configuration_directory = workspace / "configuration"
        self.assertIn(f"Include = {configuration_directory}/mirrorlist\n", preparer.configuration)
        self.assertIn(f"Server = file://{plugin}/fixtures/$repo\n", preparer.configuration)
        self.assertIn("[custom]\n", preparer.configuration)
        self.assertNotIn("[rstarsleep]", preparer.configuration)
        self.assertNotIn("file:///stale", preparer.configuration)
        self.assertEqual(preparer.official_conf.read_text(), preparer.configuration)
        self.assertEqual(preparer.build_conf.read_text(), preparer.configuration)
        self.assertEqual((configuration_directory / "mirrorlist").read_text(),
                         "Server = https://default.invalid/$repo/os/$arch\n")
        self.assertEqual(workspace.stat().st_mode & 0o777, 0o755)
        self.assertEqual(configuration_directory.stat().st_mode & 0o777, 0o755)
        for path in (configuration_directory / "pacman.conf", configuration_directory / "mirrorlist", preparer.makepkg_conf):
            self.assertEqual(path.stat().st_mode & 0o777, 0o644)

    def test_preparer_honors_both_mirror_parameter_interfaces(self):
        for variable in ("RSTAR_DISTRIBUTION_MIRROR", "RSTAR_MIRROR"):
            with self.subTest(variable=variable):
                plugin, workspace = self.fixture()
                mirror = "https://mirror.invalid/$repo/os/$arch"
                self.initialize(plugin, workspace, {variable: mirror})
                self.assertEqual((workspace / "configuration/mirrorlist").read_text(), f"Server = {mirror}\n")

    def test_missing_plugin_template_fails_without_using_system_configuration(self):
        plugin, workspace = self.fixture()
        (plugin / "data/pacman.conf.in").unlink()
        with self.assertRaisesRegex(arch.PreparationError, "command failed"):
            self.initialize(plugin, workspace)
        self.assertFalse((workspace / "makepkg.conf").exists())

    @unittest.skipUnless(shutil.which("pacman-conf"), "pacman-conf unavailable")
    def test_preparer_includes_only_enabled_explicit_keyring_roots(self):
        plugin, workspace = self.fixture()
        (plugin / "data/repository-keyrings.conf").write_text(
            "[custom]\nkeyring_package = custom-keyring\nbootstrap_keys = " + "A" * 40 + "\n"
            "[disabled]\nkeyring_package = disabled-keyring\nbootstrap_keys = " + "B" * 40 + "\n")
        preparer = self.initialize(plugin, workspace)
        self.assertEqual(preparer.keyring_targets, ["custom/custom-keyring"])
        self.assertFalse((plugin / "state/repository/bootstrap-keys").exists())


class SaveProgress(unittest.TestCase):
    def preparer(self, root):
        p = arch.Preparer.__new__(arch.Preparer)
        p.repo = root / "repo"
        p.repo.mkdir()
        value = dict(format=1, architecture="x86_64", packages={"fixture": {"sha256": "a" * 64}})
        p.verification = Mock(path=p.repo / ".official-verifications.json")
        p.verification.value.return_value = value
        for marker in (".prepared.json", ".planned.json"):
            (p.repo / marker).write_text('{"old":true}')
        return p

    def managed_save(self, p, root, success=True):
        observed = {}
        address = root / "control.sock"
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as listener:
            listener.bind(str(address))
            listener.listen(1)
            listener.settimeout(3)

            def serve():
                with listener.accept()[0] as connection:
                    connection.settimeout(3)
                    observed["request"] = json.loads(connection.makefile("rb").readline())
                    observed["markers"] = [(p.repo / name).exists() for name in (".prepared.json", ".planned.json")]
                    observed["verification"] = json.loads(p.verification.path.read_text())
                    response = dict(id=1, ok=success)
                    if not success:
                        response["error"] = "disk full"
                    connection.sendall(json.dumps(response).encode() + b"\n")

            thread = threading.Thread(target=serve, daemon=True)
            thread.start()
            sdk = Path(__file__).resolve().parents[1] / "src/controller/plugins/sdk"
            try:
                with patch.dict(os.environ, {"RSTAR_CONTROL_SOCKET": str(address), "RSTAR_SDK_DIR": str(sdk)}):
                    p.save_progress()
            finally:
                thread.join(timeout=4)
                self.assertFalse(thread.is_alive())
        return observed

    def test_real_sdk_saves_verification_without_old_completion_markers(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            p = self.preparer(root)
            observed = self.managed_save(p, root)
            self.assertEqual(observed["request"], dict(version=1, id=1, operation="save"))
            self.assertEqual(observed["markers"], [False, False])
            self.assertEqual(observed["verification"], p.verification.value())

    def test_save_error_aborts_preparation(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            p = self.preparer(root)
            with self.assertRaisesRegex(arch.PreparationError, "disk full"):
                self.managed_save(p, root, success=False)

    def test_control_channel_without_sdk_is_an_error(self):
        with tempfile.TemporaryDirectory() as temporary:
            p = self.preparer(Path(temporary))
            with patch.dict(os.environ, {"RSTAR_CONTROL_SOCKET": "socket", "RSTAR_SDK_DIR": ""}):
                with self.assertRaisesRegex(arch.PreparationError, "SDK directory"):
                    p.save_progress()

    def test_old_controller_keeps_whole_call_transaction_behavior(self):
        with tempfile.TemporaryDirectory() as temporary:
            p = self.preparer(Path(temporary))
            with patch.dict(os.environ, {"RSTAR_CONTROL_SOCKET": ""}):
                p.save_progress()
            self.assertTrue((p.repo / ".prepared.json").exists())
            self.assertFalse(p.verification.path.exists())


class Protocol(unittest.TestCase):
    def preparer(self, cache, packages):
        p = arch.Preparer.__new__(arch.Preparer)
        p.cache = Path(cache)
        p.keyring_targets = []
        p.files, p.resolutions = set(), {}
        p.verification = Mock(reusable=Mock(return_value=False))
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

    def test_download_batch_is_saved_before_a_later_batch_fails(self):
        with tempfile.TemporaryDirectory() as temporary:
            p = self.preparer(temporary, [("library-1", "library", "1-1"), ("library-2", "library", "2-1")])
            saved = []
            p.save_progress = Mock(side_effect=lambda: saved.append(set(p.files)))

            def pacman(arguments, **kwargs):
                if arguments[-1] == "library=2-1":
                    raise arch.PreparationError("second batch failed")
                (p.cache / "library-1").write_text("verified package")

            p.pacman = Mock(side_effect=pacman)
            with self.assertRaisesRegex(arch.PreparationError, "second batch failed"):
                p.fetch(["library-1", "library-2"])
            self.assertEqual(saved, [{"library-1"}])
            p.verification.verified.assert_called_once_with("library-1", p.official_metadata["library-1"], p.cache / "library-1")

    def test_invalid_download_is_never_saved(self):
        with tempfile.TemporaryDirectory() as temporary:
            p = self.preparer(temporary, [("pkg", "pkg", "1-1")])
            (p.cache / "pkg").write_text("bad package")
            p.pacman = Mock()
            p.save_progress = Mock()
            p.verification.verified.side_effect = arch.official_cache.VerificationError("bad digest")
            with self.assertRaisesRegex(arch.PreparationError, "bad digest"):
                p.fetch(["pkg"])
            p.save_progress.assert_not_called()
            self.assertFalse(p.files)

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
            p.arch, p.aur_devel, p.aur_rebuild = "x86_64", True, set()
            p.config_sha256, p.cache_counts = "fixture", dict(reused=0, built=0)
            p.builder = Mock(pw_uid=os.getuid(), pw_gid=os.getgid())
            p.outputs, p.repo, p.cache = root / "outputs", root / "repo", root / "cache"
            for directory in (p.outputs, p.repo, p.cache):
                directory.mkdir()
            p.makepkg_conf = root / "makepkg.conf"
            p.makepkg_conf.write_text("# configuration\n")
            p.ensure, p.publish, p.prefetch = Mock(), Mock(), Mock()
            p.official = Mock(return_value=[])
            calls = []

            def makepkg(checkout, arguments):
                calls.append(arguments)
                if "--force" in arguments:
                    for name in ("sample-cli", "sample-gui", "sample-cli-debug"):
                        (p.outputs / "sample" / f"{name}-dynamic.pkg.tar.zst").write_text("binary")

            def inspect(command, **kwargs):
                package = Path(command[2]).name.split("-dynamic")[0]
                return subprocess.CompletedProcess(command, 0,
                    f"pkgname = {package}\npkgver = 2.r15-1\narch = x86_64\ndepend = generated-runtime\n"
                    "provides = generated-provider=2\nconflict = obsolete-provider\n", "")

            p.makepkg = makepkg
            base = arch.srcinfo(RECIPE, "x86_64")
            base.update(checkout=root, commit="current-recipe")
            with patch.object(arch, "run", side_effect=inspect):
                p.build(base)
            self.assertEqual(len(p.artifacts["sample"]), 3)
            self.assertEqual(p.states["sample"], "complete")
            self.assertEqual(base["version"], "2.r15-1")
            self.assertEqual(base["runtime_by_package"]["sample-cli"], ["generated-runtime"])
            self.assertEqual(base["provides"]["sample-cli"], ["generated-provider=2"])
            self.assertEqual(base["conflicts"]["sample-cli"], ["obsolete-provider"])
            self.assertIn(["--force", "--cleanbuild", "--clean", "--holdver", "--nodeps", "--noconfirm"], calls)
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
            p.verification = Mock(context={"fingerprint": "fixture", "expires": 0}, expired=Mock(return_value=False))
            p.verification.read_context.return_value = p.verification.context
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

    def test_forced_aur_wins_over_same_named_repository_package(self):
        p = self.plan_preparer([("base", "1-1", {}), ("paru", "9-1", {})])
        p.planned_transactions = {}
        recipe = arch.srcinfo("pkgbase = paru\npkgver = 1\npkgrel = 1\npkgname = paru\n", "x86_64")
        p.aur = Mock(return_value=recipe)
        p.fetch, p.finish, p.clean_dependencies, p.ensure = Mock(), Mock(), Mock(), Mock()
        result = arch.prepare_systems(p, {"desktop": ["aur/paru"]})
        self.assertEqual(result["systems"]["desktop"]["packages"], ["base", "paru"])
        p.ensure.assert_called_once_with("aur/paru")
        self.assertTrue(any("/paru" in value and value != "aur/paru"
                            for value in p.pacman.call_args.args[0]))
        self.assertNotIn("paru-9-1-x86_64.pkg.tar.zst", p.fetch.call_args.args[0])

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

    def test_shared_preparation_resolves_conflicting_systems_once_each_and_downloads_union(self):
        p = self.plan_preparer([
            ("base", "1-1", {"DEPENDS": ["core"]}),
            ("core", "1-1", {}),
            ("alpha", "1-1", {"CONFLICTS": ["beta"], "DEPENDS": ["shared"]}),
            ("beta", "1-1", {"CONFLICTS": ["alpha"], "DEPENDS": ["shared"]}),
            ("shared", "1-1", {}),
        ])
        p.planned_transactions = {}
        p.fetch, p.finish = Mock(), Mock()
        p.aur = Mock(side_effect=AssertionError("official targets became AUR requests"))
        result = arch.prepare_systems(p, {"desktop": ["alpha"], "server": ["beta"], "minimal": []})
        self.assertEqual(result["systems"], {
            "desktop": {"schema_version": 1, "packages": ["alpha", "base", "core", "shared"]},
            "server": {"schema_version": 1, "packages": ["base", "beta", "core", "shared"]},
            "minimal": {"schema_version": 1, "packages": ["base", "core"]},
        })
        self.assertEqual(p.pacman.call_count, 3, "one complete transaction per system, no per-target probing")
        p.fetch.assert_called_once()
        self.assertEqual(p.fetch.call_args.args[0], set(p.official_metadata))
        p.finish.assert_called_once_with(["alpha", "base", "beta"])

    def test_preparation_reports_the_system_whose_transaction_fails(self):
        p = self.plan_preparer([
            ("base", "1-1", {}),
            ("broken", "1-1", {"DEPENDS": ["absent"]}),
        ])
        p.planned_transactions = {}
        p.fetch, p.finish = Mock(), Mock()
        with self.assertRaisesRegex(arch.PreparationError, "system desktop"):
            arch.prepare_systems(p, {"desktop": ["broken"]})
        p.fetch.assert_not_called()
        p.finish.assert_not_called()

    def test_final_aur_plan_uses_binary_runtime_dependencies_without_build_dependencies(self):
        p = self.plan_preparer([
            ("base", "1-1", {}), ("old-runtime", "1-1", {}),
            ("new-runtime", "1-1", {}), ("compiler", "1-1", {}),
        ])
        recipe = arch.srcinfo(
            "pkgbase = aur-tool\npkgver = 1\npkgrel = 1\nmakedepends = compiler\n"
            "pkgname = aur-tool\ndepends = old-runtime\n", "x86_64")
        p.planned_transactions = {}
        p.aur = Mock(return_value=recipe)
        p.fetch, p.finish, p.clean_dependencies = Mock(), Mock(), Mock()
        def build_binary(target):
            self.assertEqual(target, "aur-tool")
            recipe["runtime_by_package"]["aur-tool"] = ["new-runtime"]
            recipe["version"] = "2-1"
        p.ensure = Mock(side_effect=build_binary)
        result = arch.prepare_systems(p, {"desktop": ["aur-tool"]})
        self.assertEqual(result["systems"]["desktop"]["packages"], ["aur-tool", "base", "new-runtime"])
        self.assertEqual(p.pacman.call_count, 2, "AUR gets one source plan and one binary plan")
        p.ensure.assert_called_once_with("aur-tool")
        self.assertNotIn("compiler", result["systems"]["desktop"]["packages"])

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
            p.verification = Mock(reusable=Mock(return_value=False))
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
    def test_keyring_closure_is_downloaded_once_and_retained_in_every_system_plan(self):
        preparer = self.preparer({"base": ["base"], "custom/custom-keyring": ["custom-keyring", "keyring-dependency"]})
        preparer.keyring_targets = ["custom/custom-keyring"]
        preparer.planned_transactions = {}
        preparer.fetch = Mock()
        preparer.finish = Mock()
        result = arch.prepare_systems(preparer, {"desktop": [], "server": []})
        for plan in result["systems"].values():
            self.assertEqual(plan["packages"], ["base", "custom-keyring", "keyring-dependency"])
        preparer.fetch.assert_called_once_with({"base.pkg", "custom-keyring.pkg", "keyring-dependency.pkg"})
        preparer.finish.assert_called_once_with(["base", "custom/custom-keyring"])

    def test_all_entry_points_create_workspaces_in_plugin_temporary_directory(self):
        preparer = Mock(keyring_targets=[])
        preparer.collect.return_value = ["base"]
        preparer.prefetch.return_value = []
        workspaces = []
        with tempfile.TemporaryDirectory() as temporary:
            def initialize(workspace):
                self.assertEqual(Path(workspace).parent, Path(temporary))
                self.assertTrue(Path(workspace).is_dir())
                workspaces.append(workspace)
                return preparer

            with patch.dict(os.environ, {"RSTAR_PLUGIN_TMP": temporary}), \
                    patch.object(arch, "Preparer", side_effect=initialize), \
                    patch.object(arch, "prepare_systems", return_value={}), \
                    patch.object(arch.sys, "argv", ["predownload.py"]), \
                    redirect_stdout(StringIO()), redirect_stderr(StringIO()):
                arch.collect_main(["base"])
                arch.systems_main(json.dumps({"schema_version": 1, "systems": {"minimal": []}}))
                arch.main()
            self.assertEqual(len(workspaces), 3)
            self.assertTrue(all(not Path(workspace).exists() for workspace in workspaces))

    def test_system_preparation_keeps_download_diagnostics_out_of_json(self):
        preparer = Mock()
        preparer.initialize.side_effect = lambda: arch.run([sys.executable, "-c", "print('environment ready')"])
        result = {"schema_version": 1, "systems": {"minimal": {"schema_version": 1, "packages": ["base"]}}}
        output = StringIO()
        with tempfile.TemporaryFile(mode="w+") as diagnostics:
            with patch.object(arch, "Preparer", return_value=preparer), \
                    patch.object(arch, "prepare_systems", return_value=result) as prepare, \
                    redirect_stdout(output), redirect_stderr(diagnostics):
                arch.systems_main(json.dumps({"schema_version": 1, "systems": {"minimal": []}}))
            diagnostics.seek(0)
            self.assertEqual(diagnostics.read(), "environment ready\n")
        self.assertEqual(json.loads(output.getvalue()), result)
        prepare.assert_called_once_with(preparer, {"minimal": []})
        self.assertFalse(arch.QUERY_MODE)

    def test_query_keeps_child_diagnostics_out_of_json_stdout(self):
        preparer = Mock(keyring_targets=[])
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

    def test_planner_reuses_updated_metadata_without_refreshing_or_upgrading_again(self):
        with tempfile.TemporaryDirectory() as temporary:
            preparer = arch.Preparer.__new__(arch.Preparer)
            preparer.keyring_targets = []
            preparer.root = Path(temporary) / "plugin"
            preparer.official_metadata = {}
            preparer.db = Path(temporary) / "db"
            (preparer.db / "sync").mkdir(parents=True)
            preparer.actual_db = Path(temporary) / "actual"
            (preparer.actual_db / "sync").mkdir(parents=True)
            with tarfile.open(preparer.actual_db / "sync/fixture.db", "w:gz"):
                pass
            preparer.pacman = Mock(return_value=subprocess.CompletedProcess([], 0, "", ""))
            preparer.refresh_repo = Mock()
            preparer.initialize(planning=True)
            preparer.pacman.assert_not_called()
            self.assertEqual((preparer.db / "sync/fixture.db").read_bytes(),
                             (preparer.actual_db / "sync/fixture.db").read_bytes())
            preparer.initialize()
            self.assertEqual([call.args[0] for call in preparer.pacman.call_args_list], [["-Qq"]])

    def test_restored_environment_initializes_keyrings_before_dependency_baseline(self):
        with tempfile.TemporaryDirectory() as temporary:
            preparer = arch.Preparer.__new__(arch.Preparer)
            preparer.root = Path(temporary) / "plugin"
            preparer.official_conf = Path(temporary) / "official.conf"
            preparer.keyring_targets = ["custom/custom-keyring"]
            preparer.official_metadata = {}
            preparer.db = Path(temporary) / "db"
            preparer.actual_db = Path(temporary) / "actual"
            (preparer.actual_db / "sync").mkdir(parents=True)
            (preparer.db / "local").mkdir(parents=True)
            with tarfile.open(preparer.actual_db / "sync/fixture.db", "w:gz"):
                pass
            preparer.refresh_repo = Mock()
            events = []
            preparer.pacman = Mock(side_effect=lambda *args, **kwargs: (
                events.append("baseline"), subprocess.CompletedProcess([], 0, "base\ncustom-keyring\n", ""))[1])
            with patch.object(arch, "run", side_effect=lambda *args, **kwargs: events.append("keyrings")) as run:
                preparer.initialize()
            self.assertEqual(events, ["keyrings", "baseline"])
            self.assertEqual(preparer.baseline, {"base", "custom-keyring"})
            self.assertIn("prepare_repository_keyrings", run.call_args.args[0][2])




if __name__ == "__main__":
    unittest.main()

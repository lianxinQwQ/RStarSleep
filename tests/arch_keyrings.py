"""Hermetic checks for explicit repository trust bootstrap; no real keys or packages."""
import json
import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path


LIB = Path(__file__).resolve().parents[1] / "presets/plugins/arch/lib"
FINGERPRINT = "0123456789ABCDEF0123456789ABCDEF01234567"
OTHER_FINGERPRINT = "89ABCDEF0123456789ABCDEF0123456789ABCDEF"

# One executable supplies all external trust/package operations. Arguments are
# logged as JSON so spaces and punctuation cannot obscure which paths were used.
MOCK = r'''#!/usr/bin/env python3
import json
import os
import sys
from pathlib import Path

name, args = Path(sys.argv[0]).name, sys.argv[1:]
root = Path(os.environ["KEYRING_TEST_ROOT"])
with (root / "trace").open("a") as stream:
    stream.write(json.dumps([name, args]) + "\n")
present = root / "key-present"
fingerprint = os.environ["KEYRING_TEST_FINGERPRINT"]
if name == "pacman-conf":
    if "--repo-list" in args:
        print("core")
        if os.environ.get("KEYRING_TEST_DISABLED") != "1":
            print("custom")
    elif any(value.lower() == "gpgdir" for value in args):
        print(root / "isolated gnupg")
    elif "SigLevel" in args:
        variable = "KEYRING_TEST_REPO_SIGLEVEL" if "--repo" in args else "KEYRING_TEST_GLOBAL_SIGLEVEL"
        value = os.environ.get(variable, "" if "--repo" in args else "PackageRequired PackageTrustedOnly DatabaseOptional DatabaseTrustedOnly")
        if value:
            print("\n".join(value.split()))
    else:
        sys.exit(81)
elif name == "gpg":
    if "--recv-keys" in args:
        if os.environ.get("KEYRING_TEST_NETWORK_FAILURE") == "1":
            sys.exit(82)
        present.touch()
    elif "--import" in args and "--dry-run" not in args:
        present.touch()
    elif "--export" in args:
        if "--output" in args:
            Path(args[args.index("--output") + 1]).write_text("fixture public key\n")
        else:
            print("fixture public key")
    elif any(value in args for value in ("--list-keys", "--list-public-keys", "--show-keys")):
        if not present.exists() and "--show-keys" not in args:
            sys.exit(2)
        status = os.environ.get("KEYRING_TEST_KEY_STATUS", "u")
        actual = os.environ.get("KEYRING_TEST_ACTUAL_FINGERPRINT", fingerprint)
        # GnuPG's first fpr after pub identifies the primary key. A second
        # fingerprint deliberately exercises primary/subkey discrimination.
        print("pub:{}:2048:1:{}:1700000000:0::u:::scESC:".format(status, actual[-16:]))
        print("fpr:::::::::{}:".format(actual))
        print("sub:u:2048:1:AAAAAAAAAAAAAAAA:1700000000:0:::::s:")
        print("fpr:::::::::AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA:")
elif name == "pacman-key":
    baseline = args == ["--init"] or args == ["--populate", "archlinux"]
    if not (baseline and os.environ.get("KEYRING_TEST_BASE_INSTALL") == "1") and (
            "--gpgdir" not in args or args[args.index("--gpgdir") + 1] != str(root / "isolated gnupg")):
        print("test blocked a nonisolated keyring operation", file=sys.stderr)
        sys.exit(83)
    if "--recv-keys" in args:
        if os.environ.get("KEYRING_TEST_NETWORK_FAILURE") == "1":
            sys.exit(82)
        present.touch()
    elif "--add" in args:
        present.touch()
elif name == "pacman":
    inventory_path = root / "inventory.json"
    inventory = json.loads(inventory_path.read_text()) if inventory_path.exists() else None
    targets = args[args.index("--") + 1:] if "--" in args else []
    if "-Qlq" in args or "-Qql" in args:
        print("/usr/share/pacman/keyrings/custom.gpg")
        print("/usr/share/pacman/keyrings/custom-trusted")
        print("/usr/share/pacman/keyrings/custom-revoked")
    elif "-Si" in args:
        print("Name : " + targets[0].split("/")[-1])
    elif "-Qq" in args and inventory is not None:
        print("\n".join(inventory["installed"]))
    elif "-Qqe" in args and inventory is not None:
        print("\n".join(inventory["explicit"]))
    elif "-Qdtq" in args and inventory is not None:
        orphans = [value for value in inventory.get("orphans", [])
                   if value in inventory["installed"] and value not in inventory["explicit"]]
        if orphans:
            print("\n".join(orphans))
    elif "-D" in args and inventory is not None:
        if "--asexplicit" in args:
            inventory["explicit"] = list(dict.fromkeys([*inventory["explicit"], *targets]))
        elif "--asdeps" in args:
            inventory["explicit"] = [value for value in inventory["explicit"] if value not in targets]
        inventory_path.write_text(json.dumps(inventory))
    elif any(value in args for value in ("-R", "-Rns")) and inventory is not None:
        inventory["installed"] = [value for value in inventory["installed"] if value not in targets]
        inventory["explicit"] = [value for value in inventory["explicit"] if value not in targets]
        inventory_path.write_text(json.dumps(inventory))
    elif "-Sy" in args and os.environ.get("KEYRING_TEST_SYNC_FAILURE") == "1":
        sys.exit(84)
    elif any(value in args for value in ("-S", "-Sy", "-Su", "-Syu")) and os.environ.get("KEYRING_TEST_PACKAGE_FAILURE") == "1":
        sys.exit(85)
    elif any(value in args for value in ("-S", "-Sy", "-Su", "-Syu")) and inventory is not None:
        for target in targets:
            package = target.split("/")[-1].split("=", 1)[0]
            if package not in inventory["installed"]:
                inventory["installed"].append(package)
                inventory["explicit"].append(package)
            if package == "custom-keyring" and "keyring-dependency" not in inventory["installed"]:
                inventory["installed"].append("keyring-dependency")
        inventory_path.write_text(json.dumps(inventory))
'''


class RepositoryKeyrings(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="arch keyring ")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.plugin = self.root / "plugin with spaces"
        for relative in ("lib", "data", "state/packages", "state/repository"):
            (self.plugin / relative).mkdir(parents=True, exist_ok=True)
        for name in ("common.sh", "keyrings.sh", "maintenance.sh"):
            shutil.copyfile(LIB / name, self.plugin / "lib" / name)
        for name in ("resolve_packages.sh", "maintain_packages.sh", "base_install.sh", "query_packages.sh"):
            shutil.copyfile(LIB.parent / name, self.plugin / name)
        self.binary = self.root / "bin"
        self.binary.mkdir()
        for command in ("gpg", "pacman-key", "pacman-conf", "pacman"):
            path = self.binary / command
            path.write_text(MOCK)
            path.chmod(0o755)
        (self.root / "isolated gnupg").mkdir(mode=0o700)
        self.config = self.plugin / "data/repository-keyrings.conf"
        self.config.write_text(self.configuration())
        self.pacman_conf = self.root / "pacman.conf"
        self.pacman_conf.write_text("[options]\n[core]\n[custom]\n")
        (self.plugin / "data/pacman.conf.in").write_text(
            "[options]\nSigLevel = Required DatabaseOptional\n"
            f"GPGDir = {self.root / 'isolated gnupg'}\n[core]\nInclude = @MIRRORLIST@\n"
            "[custom]\nServer = https://custom.invalid/$arch\n")
        (self.plugin / "data/mirrorlist").write_text("Server = https://mirror.invalid/$repo/os/$arch\n")

    def configuration(self, fingerprint=FINGERPRINT):
        return f"[custom]\nkeyring_package = custom-keyring\nbootstrap_keys = {fingerprint}\n"

    def environment(self, **variables):
        environment = os.environ.copy()
        environment.update(PATH=f"{self.binary}:{environment['PATH']}",
                           RSTAR_PLUGIN_ROOT=str(self.plugin),
                           RSTAR_PLUGIN_TMP=str(self.root),
                           RSTAR_SYSTEM_ID="predownload",
                           KEYRING_TEST_ROOT=str(self.root),
                           KEYRING_TEST_FINGERPRINT=FINGERPRINT)
        environment.update(variables)
        return environment

    def run_helper(self, mode="refresh", **variables):
        return subprocess.run(["bash", "-c",
                               'set -euo pipefail; source "$1/common.sh"; source "$1/keyrings.sh"; '
                               'prepare_repository_keyrings "$2" "$3"',
                               "keyring-test", str(LIB), str(self.pacman_conf), mode],
                              env=self.environment(**variables), text=True, capture_output=True, timeout=15)

    def run_script(self, name, arguments=(), **variables):
        return subprocess.run(["bash", str(self.plugin / name), *arguments],
                              env=self.environment(**variables), text=True, capture_output=True, timeout=20)

    def inventory(self, obsolete=False):
        value = dict(installed=["base", "custom-keyring", "keyring-dependency"],
                     explicit=["base", "custom-keyring"], orphans=[])
        if obsolete:
            value["installed"].append("old-bootstrap-tool")
            value["explicit"].append("old-bootstrap-tool")
            value["orphans"].append("old-bootstrap-tool")
        (self.root / "inventory.json").write_text(json.dumps(value))
        return value

    def targets(self, filter_enabled=False):
        environment = os.environ.copy()
        environment.update(PATH=f"{self.binary}:{environment['PATH']}",
                           KEYRING_TEST_ROOT=str(self.root),
                           KEYRING_TEST_FINGERPRINT=FINGERPRINT)
        return subprocess.run(["bash", "-c",
                               'set -euo pipefail; source "$1/common.sh"; source "$1/keyrings.sh"; '
                               'repository_keyring_targets "$2" "$3"',
                               "keyring-target-test", str(LIB), str(self.config),
                               str(self.pacman_conf) if filter_enabled else ""],
                              env=environment, text=True, capture_output=True, timeout=15)

    def trace(self):
        path = self.root / "trace"
        return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []

    def operations(self, command, option=None):
        return [(index, args) for index, (name, args) in enumerate(self.trace())
                if name == command and (option is None or option in args)]

    def assert_success(self, result):
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_configuration_returns_explicit_repository_targets(self):
        self.config.write_text(self.configuration().replace(FINGERPRINT, FINGERPRINT.lower())
                               + "keyserver = hkps://keys.example.invalid\n")
        result = self.targets()
        self.assert_success(result)
        self.assertEqual(result.stdout.splitlines(), ["custom/custom-keyring"])

    def test_rejects_incomplete_ambiguous_and_executable_configuration(self):
        injected = self.root / "injected"
        cases = [
            self.configuration("F9F9FA97A403F63E"),
            self.configuration(""),
            "[custom]\nbootstrap_keys = " + FINGERPRINT + "\n",
            self.configuration() + "keyring_package = other-keyring\n",
            self.configuration() + self.configuration(),
            self.configuration() + "unknown_setting = ignored\n",
            self.configuration().replace("custom-keyring", "../custom-keyring"),
            self.configuration().replace("custom-keyring", f'$(touch "{injected}")'),
        ]
        for text in cases:
            with self.subTest(configuration=text):
                self.config.write_text(text)
                result = self.targets()
                self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertFalse(injected.exists())

    def test_disabled_repository_keyring_is_not_a_target_plan_root(self):
        self.config.write_text(self.configuration()
                               + self.configuration().replace("custom", "disabled"))
        result = self.targets(filter_enabled=True)
        self.assert_success(result)
        self.assertEqual(result.stdout.splitlines(), ["custom/custom-keyring"])

    def test_initialization_trust_precedes_database_refresh_and_install(self):
        result = self.run_helper()
        self.assert_success(result)
        receive, = self.operations("pacman-key", "--recv-keys")
        sign, = self.operations("pacman-key", "--lsign-key")
        refresh, = self.operations("pacman", "-Sy")
        populate, = self.operations("pacman-key", "--populate")
        self.assertLess(receive[0], sign[0])
        self.assertLess(sign[0], refresh[0])
        self.assertLess(refresh[0], populate[0])
        self.assertIn("custom/custom-keyring", refresh[1])
        self.assertIn("--needed", refresh[1])
        self.assertIn("--noconfirm", refresh[1])
        self.assertIn("custom", populate[1])

    def test_cached_public_key_bootstraps_new_environment_without_network(self):
        self.assert_success(self.run_helper())
        (self.root / "key-present").unlink()
        (self.root / "trace").write_text("")
        result = self.run_helper("reuse", KEYRING_TEST_NETWORK_FAILURE="1")
        self.assert_success(result)
        self.assertFalse(self.operations("pacman-key", "--recv-keys"))
        self.assertFalse(self.operations("pacman", "-Sy"))
        self.assertTrue(self.operations("pacman-key", "--populate"))

    def test_existing_primary_key_is_validated_without_network(self):
        (self.root / "key-present").touch()
        result = self.run_helper("reuse", KEYRING_TEST_NETWORK_FAILURE="1")
        self.assert_success(result)
        self.assertFalse(self.operations("pacman-key", "--recv-keys"))
        self.assertFalse(self.operations("pacman-key", "--add"))
        self.assertTrue(self.operations("pacman-key", "--lsign-key"))
        self.assertTrue(self.operations("pacman-key", "--populate"))

    def test_disabled_repository_does_not_establish_trust(self):
        self.run_helper(KEYRING_TEST_DISABLED="1")
        self.assertFalse(self.operations("gpg"))
        self.assertFalse(self.operations("pacman-key"))
        self.assertFalse(self.operations("pacman"))

    def test_unconfigured_repositories_do_not_add_default_trust(self):
        self.config.write_text("# No repository trust configured.\n")
        self.assert_success(self.run_helper())
        self.assertFalse(self.operations("gpg"))
        self.assertFalse(self.operations("pacman-key"))
        self.assertFalse(self.operations("pacman"))

    def test_wrong_primary_fingerprint_cannot_be_locally_signed(self):
        result = self.run_helper(KEYRING_TEST_ACTUAL_FINGERPRINT=OTHER_FINGERPRINT)
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(self.operations("pacman-key", "--lsign-key"))
        self.assertFalse(self.operations("pacman"))

    def test_revoked_expired_and_disabled_keys_are_rejected(self):
        for status in ("r", "e", "d"):
            with self.subTest(status=status):
                (self.root / "key-present").touch()
                (self.root / "trace").write_text("")
                result = self.run_helper(KEYRING_TEST_KEY_STATUS=status)
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse(self.operations("pacman-key", "--lsign-key"))
                self.assertFalse(self.operations("pacman"))

    def test_network_failure_stops_before_trust_and_package_operations(self):
        result = self.run_helper(KEYRING_TEST_NETWORK_FAILURE="1")
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(self.operations("pacman-key", "--lsign-key"))
        self.assertFalse(self.operations("pacman"))

    def test_database_refresh_failure_stops_before_install_and_populate(self):
        result = self.run_helper(KEYRING_TEST_SYNC_FAILURE="1")
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(self.operations("pacman", "-S"))
        self.assertFalse(self.operations("pacman-key", "--populate"))

    def test_package_failure_stops_before_populate(self):
        result = self.run_helper(KEYRING_TEST_PACKAGE_FAILURE="1")
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(self.operations("pacman-key", "--populate"))

    def test_untrusted_or_optional_global_package_policy_is_rejected_before_mutation(self):
        for policy in ("PackageNever DatabaseNever", "PackageOptional PackageTrustedOnly",
                       "PackageRequired PackageTrustAll", ""):
            with self.subTest(policy=policy):
                (self.root / "trace").write_text("")
                result = self.run_helper(KEYRING_TEST_GLOBAL_SIGLEVEL=policy)
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse(self.operations("gpg"))
                self.assertFalse(self.operations("pacman-key"))
                self.assertFalse(self.operations("pacman"))

    def test_weak_repository_override_is_rejected_despite_strong_global_policy(self):
        result = self.run_helper(KEYRING_TEST_REPO_SIGLEVEL="PackageRequired PackageTrustAll DatabaseOptional")
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(self.operations("gpg"))
        self.assertFalse(self.operations("pacman-key"))
        self.assertFalse(self.operations("pacman"))

    def test_trusted_required_packages_allow_optional_or_unsigned_database_policy(self):
        result = self.run_helper(KEYRING_TEST_REPO_SIGLEVEL="PackageRequired PackageTrustedOnly DatabaseNever")
        self.assert_success(result)
        self.assertTrue(self.operations("pacman-key", "--populate"))

    def test_wrapper_prepares_keyring_before_upgrade_and_does_not_refresh_twice(self):
        result = subprocess.run(["bash", "-c", 'source "$1"; pacman -Syu --noconfirm -- base',
                                 "keyring-wrapper-test", str(self.plugin / "lib/common.sh")],
                                env=self.environment(), text=True, capture_output=True, timeout=20)
        self.assert_success(result)
        sync, = self.operations("pacman", "-Sy")
        upgrade, = self.operations("pacman", "-Su")
        populate, = self.operations("pacman-key", "--populate")
        self.assertLess(sync[0], populate[0])
        self.assertLess(populate[0], upgrade[0])
        self.assertIn("base", upgrade[1])
        self.assertFalse(self.operations("pacman", "-Syu"))

    def test_base_install_keeps_keyring_explicit_and_cleans_unrelated_bootstrap_orphan(self):
        self.inventory(obsolete=True)
        result = self.run_script("base_install.sh", KEYRING_TEST_BASE_INSTALL="1")
        self.assert_success(result)
        inventory = json.loads((self.root / "inventory.json").read_text())
        self.assertEqual(set(inventory["installed"]), {"base", "custom-keyring", "keyring-dependency"})
        self.assertEqual(set(inventory["explicit"]), {"base", "custom-keyring"})
        for _, arguments in self.operations("pacman", "-D"):
            if "--asdeps" in arguments:
                self.assertNotIn("custom-keyring", arguments)

    def test_maintenance_preserves_keyring_source_and_root_without_requesting_it(self):
        self.inventory()
        result = self.run_script("maintain_packages.sh",
                                 ["--plan", "base", "custom-keyring", "keyring-dependency", "--targets"])
        self.assert_success(result)
        state = json.loads(result.stdout)
        self.assertEqual(state["requested"], [])
        self.assertEqual({item["name"]: item["explicit"] for item in state["installed"]},
                         {"base": True, "custom-keyring": True, "keyring-dependency": False})
        transactions = [arguments for _, arguments in self.operations("pacman", "-S") if "--ask=4" in arguments]
        self.assertEqual(len(transactions), 1)
        self.assertIn("custom/custom-keyring", transactions[0])
        self.assertNotIn("custom-keyring", transactions[0])
        self.assertNotIn("base", transactions[0])
        upgrade, = self.operations("pacman", "-Su")
        self.assertNotIn("--", upgrade[1])
        self.assertFalse(self.operations("pacman", "-R"))

    def test_legacy_base_capture_resolves_only_base_and_does_not_bootstrap_keys(self):
        inventory = dict(installed=["base", "base-dependency"], explicit=["base"], orphans=[])
        inventory_path = self.root / "inventory.json"
        inventory_path.write_text(json.dumps(inventory))
        before = inventory_path.read_bytes()
        query = self.run_script("query_packages.sh")
        self.assert_success(query)
        installed = dict(line.split("\t") for line in query.stdout.splitlines())
        resolved = self.run_script("resolve_packages.sh", ["base"])
        self.assert_success(resolved)
        self.assertEqual(resolved.stdout.splitlines(), ["base"])
        self.assertTrue(all(installed.get(package) == "explicit" for package in resolved.stdout.splitlines()))
        self.assertEqual(inventory_path.read_bytes(), before)
        self.assertFalse(self.operations("gpg"))
        self.assertFalse(self.operations("pacman-key"))
        self.assertFalse(self.operations("pacman", "-S"))
        self.assertFalse(self.operations("pacman", "-Sy"))

    def test_maintenance_upgrades_legacy_base_with_missing_internal_keyring(self):
        inventory = dict(installed=["base", "base-dependency"], explicit=["base"], orphans=[])
        (self.root / "inventory.json").write_text(json.dumps(inventory))
        result = self.run_script("maintain_packages.sh", ["--plan", "base", "base-dependency",
                                 "custom-keyring", "keyring-dependency", "desktop", "--targets", "desktop"])
        self.assert_success(result)
        state = json.loads(result.stdout)
        self.assertEqual(state["requested"], ["desktop"])
        self.assertEqual({item["name"]: item["explicit"] for item in state["installed"]},
                         {"base": True, "base-dependency": False, "desktop": True,
                          "custom-keyring": True, "keyring-dependency": False})
        populates = self.operations("pacman-key", "--populate")
        self.assertEqual(len(populates), 2)
        transactions = [(index, arguments) for index, arguments in self.operations("pacman", "-S")
                        if "desktop" in arguments]
        self.assertEqual(len(transactions), 1)
        self.assertLess(populates[0][0], transactions[0][0])
        self.assertIn("custom/custom-keyring", transactions[0][1])
        upgrade, = self.operations("pacman", "-Su")
        self.assertLess(transactions[0][0], populates[1][0])
        self.assertLess(populates[1][0], upgrade[0])

    def test_explicit_user_keyring_target_remains_requested(self):
        self.inventory()
        result = self.run_script("maintain_packages.sh", ["--plan", "base", "custom-keyring",
                                 "keyring-dependency", "--targets", "custom/custom-keyring"])
        self.assert_success(result)
        state = json.loads(result.stdout)
        self.assertEqual(state["requested"], ["custom-keyring"])
        self.assertTrue(next(item["explicit"] for item in state["installed"] if item["name"] == "custom-keyring"))


@unittest.skipUnless(shutil.which("gpg"), "GnuPG is unavailable")
class RealPrimaryFingerprints(unittest.TestCase):
    """Exercise primary, subkey and disabled colon records produced by real GnuPG."""

    def test_primary_is_accepted_and_subkey_or_disabled_primary_is_rejected(self):
        with tempfile.TemporaryDirectory(prefix="arch keyring gpg ") as temporary:
            home = Path(temporary)
            home.chmod(0o700)
            gpg = [shutil.which("gpg"), "--no-options", "--homedir", str(home), "--batch",
                   "--pinentry-mode", "loopback", "--passphrase", ""]
            result = subprocess.run([*gpg, "--quick-generate-key", "RStarSleep test <fixture@example.invalid>",
                                     "ed25519", "cert", "1d"],
                                    text=True, capture_output=True, timeout=20)
            self.assertEqual(result.returncode, 0, result.stderr)
            records = subprocess.run([*gpg, "--with-colons", "--fingerprint", "--list-keys"],
                                     text=True, capture_output=True, check=True).stdout
            primary = next(line.split(":")[9] for line in records.splitlines() if line.startswith("fpr:"))
            subprocess.run([*gpg, "--quick-add-key", primary, "ed25519", "sign", "1d"],
                           text=True, capture_output=True, check=True, timeout=20)
            records = subprocess.run([*gpg, "--with-colons", "--with-subkey-fingerprint", "--list-keys"],
                                     text=True, capture_output=True, check=True).stdout
            fingerprints = [line.split(":")[9] for line in records.splitlines() if line.startswith("fpr:")]
            self.assertEqual(len(fingerprints), 2)

            def status(fingerprint):
                return subprocess.run(["bash", "-c",
                                       'set -euo pipefail; source "$1/common.sh"; '
                                       'repository_key_status "$2" "$3"',
                                       "real-keyring-test", str(LIB), str(home), fingerprint],
                                      text=True, capture_output=True, timeout=10)

            self.assertEqual(status(primary).returncode, 0)
            self.assertEqual(status(fingerprints[1]).returncode, 1)
            subprocess.run([*gpg, "--command-fd", "0", "--edit-key", primary],
                           input="disable\nquit\n", text=True, capture_output=True, check=True, timeout=10)
            self.assertEqual(status(primary).returncode, 2)
            if shutil.which("gpgconf"):
                subprocess.run([shutil.which("gpgconf"), "--homedir", str(home), "--kill", "gpg-agent"],
                               text=True, capture_output=True, timeout=10)


if __name__ == "__main__":
    unittest.main()

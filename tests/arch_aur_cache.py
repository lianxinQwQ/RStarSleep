"""Cross-invocation AUR reuse and offline Git/makepkg integration checks."""
import copy
import json
import os
import shlex
import shutil
import subprocess
import tarfile
import tempfile
import unittest
from io import BytesIO
from pathlib import Path
from unittest.mock import Mock, patch

import arch_predownload as protocol

arch = protocol.arch


class Incremental(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.generation = 0

    def recipe(self, name="sample", text=protocol.RECIPE, commit="recipe-1"):
        base = arch.srcinfo(text.replace("sample", name), "x86_64")
        base.update(checkout=self.root, commit=commit)
        return base

    def preparer(self, **options):
        self.generation += 1
        p = arch.Preparer.__new__(arch.Preparer)
        p.arch, p.aur_devel, p.aur_rebuild = "x86_64", True, set()
        p.states, p.stack, p.artifacts = {}, [], {}
        p.cache_counts, p.cache_records = dict(reused=0, built=0), None
        p.builder = Mock(pw_uid=os.getuid(), pw_gid=os.getgid())
        p.work = self.root / str(self.generation)
        p.outputs, p.builds = p.work / "output", p.work / "build"
        p.repo, p.cache = self.root / "repo", self.root / "cache"
        p.distfiles = self.root / "distfiles"
        for path in (p.outputs, p.builds, p.repo, p.cache):
            path.mkdir(parents=True, exist_ok=True)
        p.makepkg_conf = p.work / "makepkg.conf"
        p.makepkg_conf.write_text("# stable configuration\n")
        p.config_sha256 = arch.Preparer.configuration_fingerprint(p.makepkg_conf.read_text())
        p.ensure, p.prefetch, p.publish = Mock(), Mock(), Mock()
        p.official = Mock(return_value=[])
        p.user = Mock(side_effect=AssertionError("unexpected source command"))
        p.makepkg = Mock(side_effect=lambda checkout, args: self.compile(p, args))
        p.files, p.official_metadata = set(), {}
        p.verification = arch.official_cache.OfficialPackageCache(
            p.repo, p.arch, p.work / "official.conf", arch.run, p.file_digest)
        p.save_sync = Mock()
        for key, value in options.items():
            setattr(p, key, value)
        return p

    def compile(self, p, arguments):
        if "--force" not in arguments:
            return
        for directory in p.outputs.iterdir():
            self.package(directory, directory.name + "-cli")
            self.package(directory, directory.name + "-gui")

    def package(self, directory, name, version="2.r15-1", runtime="generated-runtime", provides="generated-provider=2"):
        metadata = (f"pkgname = {name}\npkgver = {version}\narch = x86_64\n"
                    f"depend = {runtime}\nprovides = {provides}\nconflict = obsolete-provider\n")
        path = directory / f"{name}-{version}-x86_64.pkg.tar.gz"
        with tarfile.open(path, "w:gz") as archive:
            data = metadata.encode()
            entry = tarfile.TarInfo(".PKGINFO")
            entry.size = len(data)
            archive.addfile(entry, BytesIO(data))
        return path

    def test_repeat_reuses_binaries_and_only_prepares_actual_runtime_dependencies(self):
        first = self.preparer()
        first.build(self.recipe())
        second = self.preparer()
        base = self.recipe()
        second.build(base)
        second.makepkg.assert_not_called()
        second.ensure.assert_called_once_with("generated-runtime")
        second.prefetch.assert_called_once_with(["generated-runtime"])
        self.assertEqual(second.cache_counts, dict(reused=1, built=0))
        self.assertEqual(base["version"], "2.r15-1")
        self.assertEqual(base["runtime_by_package"]["sample-gui"], ["generated-runtime"])
        self.assertEqual(base["conflicts"]["sample-cli"], ["obsolete-provider"])
        second.finish(["sample-cli"])
        manifest = json.loads((second.repo / ".prepared.json").read_text())
        self.assertEqual(len(manifest["packages"]), 2)
        self.assertEqual(manifest["aur"], first.artifacts)

    def test_unselected_packages_remain_reusable(self):
        first = self.preparer()
        first.build(self.recipe())
        second = self.preparer()
        second.build(self.recipe("other"))
        third = self.preparer()
        third.build(self.recipe())
        third.makepkg.assert_not_called()
        self.assertEqual(set(third.load_cache()), {"sample", "other"})

    def test_checksum_protected_http_source_is_reused(self):
        text = "source = https://example.invalid/sample-1.tar.gz\nsha256sums = " + "a" * 64 + "\n" + protocol.RECIPE
        # source/checksum fields precede pkgbase and are still base fields.
        self.preparer().build(self.recipe(text=text))
        second = self.preparer()
        second.build(self.recipe(text=text))
        second.makepkg.assert_not_called()

    def test_failed_runtime_preparation_does_not_replace_build_record(self):
        first = self.preparer()
        first.build(self.recipe())
        previous = (first.repo / ".aur-builds.json").read_bytes()
        second = self.preparer()
        second.ensure.side_effect = lambda value, **kwargs: (_ for _ in ()).throw(
            arch.PreparationError("runtime unavailable")) if not kwargs.get("install") else None
        with self.assertRaisesRegex(arch.PreparationError, "runtime unavailable"):
            second.build(self.recipe(commit="recipe-2"))
        self.assertEqual((first.repo / ".aur-builds.json").read_bytes(), previous)

    def test_missing_or_corrupt_single_copy_is_repaired_without_rebuild(self):
        first = self.preparer()
        first.build(self.recipe())
        artifact = first.artifacts["sample"][0]
        (first.cache / artifact["filename"]).unlink()
        second = self.preparer()
        second.build(self.recipe())
        second.makepkg.assert_not_called()
        self.assertEqual(arch.Preparer.file_digest(second.cache / artifact["filename"]), artifact["sha256"])
        (second.repo / artifact["filename"]).write_bytes(b"corrupt")
        third = self.preparer()
        third.build(self.recipe())
        third.makepkg.assert_not_called()
        self.assertEqual(arch.Preparer.file_digest(third.repo / artifact["filename"]), artifact["sha256"])

    def test_missing_split_artifact_rebuilds_entire_pkgbase(self):
        first = self.preparer()
        first.build(self.recipe())
        artifact = first.artifacts["sample"][1]
        for directory in (first.repo, first.cache):
            (directory / artifact["filename"]).unlink()
        second = self.preparer()
        second.build(self.recipe())
        self.assertTrue(any("--force" in call.args[1] for call in second.makepkg.call_args_list))
        self.assertEqual(len(second.artifacts["sample"]), 2)

    def test_recipe_configuration_and_force_each_invalidate(self):
        first = self.preparer()
        first.build(self.recipe())
        for options, commit in [({}, "recipe-2"), ({"config_sha256": "different"}, "recipe-2"),
                                ({"aur_rebuild": {"sample"}}, "recipe-2"), ({"aur_rebuild": {"all"}}, "recipe-2")]:
            with self.subTest(options=options):
                p = self.preparer(**options)
                p.build(self.recipe(commit=commit))
                self.assertEqual(p.cache_counts["built"], 1)

    def test_corrupt_inventory_does_not_read_arbitrary_paths(self):
        first = self.preparer()
        first.build(self.recipe())
        index = json.loads((first.repo / ".aur-builds.json").read_text())
        index["architectures"]["x86_64"]["sample"]["artifacts"][0]["filename"] = "../outside"
        arch.atomic_json(first.repo / ".aur-builds.json", index)
        second = self.preparer()
        second.build(self.recipe())
        self.assertEqual(second.cache_counts["built"], 1)

    def test_failed_rebuild_does_not_replace_previous_record(self):
        first = self.preparer()
        first.build(self.recipe())
        previous = (first.repo / ".aur-builds.json").read_bytes()
        second = self.preparer()
        second.makepkg.side_effect = arch.PreparationError("compile failed")
        with self.assertRaisesRegex(arch.PreparationError, "compile failed"):
            second.build(self.recipe(commit="recipe-2"))
        self.assertEqual((first.repo / ".aur-builds.json").read_bytes(), previous)

    def test_git_actual_revision_is_recorded_and_changes_trigger_rebuild(self):
        base = self.recipe(text=protocol.RECIPE + "source = git+https://example.invalid/source.git\n")
        # source belongs to pkgbase in real .SRCINFO.
        base["sources"] = ["git+https://example.invalid/source.git"]
        source = base["sources"][0]
        with patch.object(arch.aur_cache, "remote_revisions", side_effect=[{source: "a" * 40}, {source: "b" * 40}]), \
                patch.object(arch.aur_cache, "actual_revisions", return_value={source: "b" * 40}):
            first = self.preparer()
            first.build(copy.deepcopy(base))
        self.assertEqual(first.load_cache()["sample"]["sources"], {source: "b" * 40})
        with patch.object(arch.aur_cache, "remote_revisions", return_value={source: "b" * 40}):
            second = self.preparer()
            second.build(copy.deepcopy(base))
            second.makepkg.assert_not_called()
        with patch.object(arch.aur_cache, "remote_revisions", return_value={source: "c" * 40}), \
                patch.object(arch.aur_cache, "actual_revisions", return_value={source: "c" * 40}):
            third = self.preparer()
            third.build(copy.deepcopy(base))
            self.assertEqual(third.cache_counts["built"], 1)

    def test_disabled_git_checks_still_reuse_and_recipe_changes_rebuild(self):
        base = self.recipe()
        base["sources"] = ["git+https://example.invalid/source.git"]
        with patch.object(arch.aur_cache, "remote_revisions", return_value={base["sources"][0]: "a" * 40}), \
                patch.object(arch.aur_cache, "actual_revisions", return_value={base["sources"][0]: "a" * 40}):
            self.preparer().build(copy.deepcopy(base))
        with patch.object(arch.aur_cache, "remote_revisions", side_effect=AssertionError("check disabled")), \
                patch.object(arch.aur_cache, "actual_revisions", return_value={base["sources"][0]: "b" * 40}):
            second = self.preparer(aur_devel=False)
            second.build(copy.deepcopy(base))
            second.makepkg.assert_not_called()
            base["commit"] = "recipe-2"
            third = self.preparer(aur_devel=False)
            third.build(copy.deepcopy(base))
            self.assertEqual(third.cache_counts["built"], 1)

    def test_failed_git_query_preserves_index_and_does_not_claim_a_hit(self):
        base = self.recipe()
        base["sources"] = ["git+https://example.invalid/source.git"]
        with patch.object(arch.aur_cache, "remote_revisions", side_effect=arch.PreparationError("unreachable")):
            p = self.preparer()
            with self.assertRaisesRegex(arch.PreparationError, "unreachable"):
                p.build(base)
            p.makepkg.assert_not_called()
            self.assertFalse((p.repo / ".aur-builds.json").exists())

    def test_unsupported_vcs_and_unchecked_http_sources_never_hit(self):
        for source in ("hg+https://example.invalid/repo", "https://example.invalid/latest.tar.gz"):
            base = self.recipe()
            base["sources"], base["unchecked_sources"] = [source], [source]
            for _ in range(2):
                p = self.preparer()
                p.build(copy.deepcopy(base))
                self.assertEqual(p.cache_counts["built"], 1)
                self.assertNotIn("sample", p.load_cache())

    def test_aur_build_dependency_change_invalidates_parent_without_installing_on_hit(self):
        dependency = self.recipe("compiler")
        first = self.preparer()
        first.build(copy.deepcopy(dependency))
        first.official = Mock(side_effect=lambda value: None if value == "compiler" else [])
        first.aur = Mock(return_value=copy.deepcopy(dependency))
        first.build(self.recipe())
        second = self.preparer()
        second.official = first.official
        second.aur = Mock(return_value=copy.deepcopy(dependency))
        second.build(self.recipe())
        second.makepkg.assert_not_called()
        self.assertFalse(any(call.kwargs.get("install") for call in second.ensure.call_args_list))
        dependency["commit"] = "compiler-2"
        third = self.preparer()
        third.official, third.aur = first.official, Mock(return_value=dependency)
        third.build(self.recipe())
        self.assertEqual(third.cache_counts["built"], 1)

    def test_versioned_soname_transition_invalidates_binary(self):
        first = self.preparer()
        first.makepkg.side_effect = lambda checkout, args: [self.package(directory, directory.name + suffix, runtime="libfoo.so=1-64")
            for directory in first.outputs.iterdir() for suffix in ("-cli", "-gui")] if "--force" in args else None
        first.build(self.recipe())
        second = self.preparer()
        second.official = Mock(side_effect=lambda value: None if value == "libfoo.so=1-64" else [])
        second.artifacts["unrelated"] = [dict(name="unrelated", version="1-1", provides=[])]
        second.build(self.recipe())
        self.assertEqual(second.cache_counts["built"], 1)

    def test_soname_provided_by_same_split_group_does_not_trigger_rebuild(self):
        first = self.preparer()
        def compile(checkout, args):
            if "--force" in args:
                self.package(first.outputs / "sample", "sample-cli", runtime="libsample.so=1-64")
                self.package(first.outputs / "sample", "sample-gui", provides="libsample.so=1-64")
        first.makepkg.side_effect = compile
        first.build(self.recipe())
        second = self.preparer()
        second.official = Mock(side_effect=lambda value: None if value == "libsample.so=1-64" else [])
        second.build(self.recipe())
        second.makepkg.assert_not_called()


class SourceTracking(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.repo = self.root / "upstream"
        self.git(["init", "--initial-branch=main", str(self.repo)])
        self.git(["-C", str(self.repo), "config", "user.name", "Offline Test"])
        self.git(["-C", str(self.repo), "config", "user.email", "offline@example.invalid"])
        self.commit("first")

    def git(self, arguments, capture=False):
        return subprocess.run(["git", *arguments], check=True, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)

    def user(self, arguments, capture=False):
        self.assertEqual(arguments[0], "git")
        return self.git(arguments[1:])

    def commit(self, content):
        (self.repo / "content").write_text(content)
        self.git(["-C", str(self.repo), "add", "content"])
        self.git(["-C", str(self.repo), "commit", "-m", content])
        return self.git(["-C", str(self.repo), "rev-parse", "HEAD"]).stdout.strip()

    def test_remote_head_branch_annotated_tag_and_fixed_commit(self):
        before = self.git(["-C", str(self.repo), "rev-parse", "HEAD"]).stdout.strip()
        self.git(["-C", str(self.repo), "tag", "-a", "v1", "-m", "release"])
        sources = [f"alias{i}::git+{self.repo.as_uri()}" + suffix for i, suffix in enumerate(
                   ("", "#branch=main", "#tag=v1?signed", "#commit=" + before))]
        specs, reason = arch.aur_cache.source_specs(sources)
        self.assertIsNone(reason)
        self.assertEqual(set(arch.aur_cache.remote_revisions(self.user, specs).values()), {before})
        after = self.commit("second")
        revisions = arch.aur_cache.remote_revisions(self.user, specs)
        self.assertEqual([revisions[source] for source in sources], [after, after, before, before])

    def test_actual_worktree_and_missing_reference(self):
        source = "alias::git+" + self.repo.as_uri()
        specs, _ = arch.aur_cache.source_specs([source])
        self.git(["clone", str(self.repo), str(self.root / "alias")])
        actual = arch.aur_cache.actual_revisions(self.user, specs, self.root)
        self.assertEqual(actual, arch.aur_cache.remote_revisions(self.user, specs))
        self.commit("second")
        self.assertNotEqual(actual, arch.aur_cache.remote_revisions(self.user, specs))
        missing, _ = arch.aur_cache.source_specs([source + "#branch=missing"])
        with self.assertRaisesRegex(ValueError, "unavailable"):
            arch.aur_cache.remote_revisions(self.user, missing)

    def test_changed_default_branch_and_moved_tag_are_resolved(self):
        mirror = self.root / "mirror"
        self.git(["clone", "--mirror", self.repo.as_uri(), str(mirror)])
        self.git(["-C", str(self.repo), "tag", "-a", "release", "-m", "first"])
        self.git(["-C", str(self.repo), "checkout", "-b", "next"])
        after = self.commit("next default branch")
        self.git(["-C", str(self.repo), "tag", "-f", "-a", "release", "-m", "moved"])
        self.git(["-C", str(mirror), "fetch", "--all"])
        sources = ["mirror::git+" + self.repo.as_uri(), "tagged::git+" + self.repo.as_uri() + "#tag=release"]
        specs, _ = arch.aur_cache.source_specs(sources)
        revisions = arch.aur_cache.remote_revisions(self.user, specs)
        self.assertEqual(set(revisions.values()), {after})
        actual = arch.aur_cache.actual_revisions(self.user, specs[:1], self.root)
        self.assertEqual(actual[sources[0]], after)

    def test_source_checksum_and_architecture_classification(self):
        base = arch.srcinfo("pkgbase = sample\npkgver = 1\npkgrel = 1\n"
            "source = https://example.invalid/fixed.tar.gz\nsha256sums = abc\n"
            "source_x86_64 = git+https://example.invalid/repo.git\nsha256sums_x86_64 = SKIP\npkgname = sample\n", "x86_64")
        specs, reason = arch.aur_cache.source_specs(base["sources"], base["unchecked_sources"])
        self.assertIsNone(reason)
        self.assertEqual(len(specs), 1)
        self.assertEqual(base["unchecked_sources"], ["git+https://example.invalid/repo.git"])
        for source in ("../escape::git+https://example.invalid/r.git", "git+https://example.invalid/r.git#commit=short",
                       "fossil+https://example.invalid/repo"):
            self.assertIsNotNone(arch.aur_cache.source_specs([source])[1])

    @unittest.skipUnless(os.geteuid() != 0 and all(shutil.which(tool) for tool in
                         ("makepkg", "fakeroot", "bsdtar", "zstd")), "unprivileged makepkg tools unavailable")
    def test_real_makepkg_git_build_reuses_then_updates_without_prepare_commit_loop(self):
        fixture = Incremental(methodName="runTest")
        fixture.setUp()
        self.addCleanup(fixture.doCleanups)
        recipe = self.root / "recipe"
        recipe.mkdir()
        (recipe / "PKGBUILD").write_text(f'''pkgname=offline-aur-cache-test
pkgver=1
pkgrel=1
pkgdesc='Offline cache verification'
arch=('any')
license=('MIT')
source=('project::git+{self.repo.as_uri()}#branch=main')
sha256sums=('SKIP')
options=('!strip' '!debug' '!lto')
prepare() {{
    cd "$srcdir/project"
    git -c user.name=Test -c user.email=test@example.invalid commit --allow-empty -m prepared
}}
pkgver() {{
    cd "$srcdir/project"
    printf '1.r%s.g%s' "$(git rev-list --count HEAD)" "$(git rev-parse --short HEAD)"
}}
package() {{
    install -Dm644 "$srcdir/project/content" "$pkgdir/usr/share/offline-aur-cache-test/content"
}}
''')
        commands = []

        def preparer():
            p = fixture.preparer()
            p.distfiles.mkdir(exist_ok=True)
            home = fixture.root / "home"
            home.mkdir(exist_ok=True)
            # Tool execution stays under the test user's identity and temp paths.
            def user(argv, capture=False):
                commands.append(argv)
                env = dict(os.environ, HOME=str(home), XDG_CONFIG_HOME=str(home / ".config"))
                result = subprocess.run(argv, capture_output=True, text=True, env=env)
                if result.returncode:
                    raise arch.PreparationError(f"offline command failed: {argv}\n{result.stdout}\n{result.stderr}")
                return result
            p.user = user
            p.makepkg = arch.Preparer.makepkg.__get__(p)
            p.makepkg_conf.write_text('source /etc/makepkg.conf\n' +
                f'SRCDEST={shlex.quote(str(p.distfiles))}\nBUILDDIR={shlex.quote(str(p.builds))}\n' +
                "BUILDENV=(!distcc !color !ccache !check !sign)\nOPTIONS=(!strip !debug !lto)\n")
            p.config_sha256 = "offline-stable-settings"
            base = arch.srcinfo(p.makepkg(recipe, ["--printsrcinfo"], capture=True).stdout, p.arch)
            base.update(checkout=recipe, commit="fixed-recipe")
            return p, base

        first, base = preparer()
        first.build(base)
        tracked_source = base["sources"][0]
        upstream = self.git(["-C", str(self.repo), "rev-parse", "HEAD"]).stdout.strip()
        self.assertEqual(first.load_cache()[base["name"]]["sources"][tracked_source], upstream)
        second, base = preparer()
        commands.clear()
        second.build(base)
        self.assertEqual(second.cache_counts, dict(reused=1, built=0))
        self.assertFalse(any("--force" in argv or "--verifysource" in argv for argv in commands))
        after = self.commit("upstream update")
        third, base = preparer()
        third.build(base)
        self.assertEqual(third.cache_counts, dict(reused=0, built=1))
        self.assertEqual(third.load_cache()[base["name"]]["sources"][tracked_source], after)
        fourth, base = preparer()
        fourth.build(base)
        self.assertEqual(fourth.cache_counts, dict(reused=1, built=0))


class Options(unittest.TestCase):
    def test_legacy_and_canonical_configuration_and_invalid_values(self):
        for prefix in ("RSTAR_", "RSTAR_DISTRIBUTION_"):
            with patch.dict(os.environ, {prefix + "AURDEVEL": "false", prefix + "AURREBUILD": "sample\nother"}, clear=True):
                self.assertEqual(arch.Preparer.cache_options(), (False, {"sample", "other"}))
        for value in ("../sample", "all sample", "none sample"):
            with patch.dict(os.environ, {"RSTAR_AURREBUILD": value}, clear=True):
                with self.assertRaises(arch.PreparationError):
                    arch.Preparer.cache_options()
        with patch.dict(os.environ, {"RSTAR_AURDEVEL": "maybe"}, clear=True):
            with self.assertRaises(arch.PreparationError):
                arch.Preparer.cache_options()


if __name__ == "__main__":
    unittest.main()

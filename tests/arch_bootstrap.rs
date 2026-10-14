//! Exercise retained bootstrap installation files without network access or real system installation.
//! 在无网络访问且不真实安装系统的条件下验证 bootstrap 安装文件保留。
#![cfg(unix)]
use sha2::{Digest, Sha256};
use std::{fs, os::unix::fs::PermissionsExt, path::Path, process::Command};

/// Create one executable mock host tool.
/// 创建一个可执行的模拟宿主工具。
/// # Arguments
/// - `path`: Isolated executable destination.
///   隔离的可执行文件目标路径。
/// - `text`: Bash source using only fixture-owned paths.
///   仅使用夹具所属路径的 Bash 源码。
fn executable(path: &Path, text: &str) {
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn missing_data_mount_fails_before_host_tools_or_downloads() {
    let temporary = tempfile::tempdir().unwrap();
    let plugin = temporary.path().join("plugin");
    fs::create_dir_all(plugin.join("lib")).unwrap();
    let preset = Path::new(env!("CARGO_MANIFEST_DIR")).join("presets/plugins/arch");
    for name in ["bootstrap.sh", "lib/common.sh", "lib/keyrings.sh"] {
        fs::copy(preset.join(name), plugin.join(name)).unwrap();
    }
    let output = Command::new("/bin/bash")
        .arg(plugin.join("bootstrap.sh"))
        .env("PATH", "/nonexistent")
        .env("RSTAR_PLUGIN_ROOT", &plugin)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("Missing persistent data mount"));
}

/// Mirror mkarchiso's root filesystem, package list and version-file layout.
/// 对应 mkarchiso 的根文件系统、软件包清单及版本文件布局。
fn bootstrap_archive(extra_path: Option<&str>) -> Vec<u8> {
    let mut archive = tar::Builder::new(Vec::new());
    let mut entries = vec![
        ("root.x86_64/etc/pacman.d/mirrorlist", "original mirror\n"),
        ("pkglist.x86_64.txt", "base 1-1\n"),
        ("version", "2026.10.01\n"),
    ];
    if let Some(path) = extra_path {
        entries.push((path, "unexpected\n"));
    }
    for (path, contents) in entries {
        let mut header = tar::Header::new_gnu();
        // Raw names permit regression fixtures containing parent-directory traversal.
        // 原始名称允许构造包含父级跳转的回归测试归档。
        header.as_mut_bytes()[..path.len()].copy_from_slice(path.as_bytes());
        header.set_mode(0o644);
        header.set_size(contents.len() as u64);
        header.set_cksum();
        archive.append(&header, contents.as_bytes()).unwrap();
    }
    archive.into_inner().unwrap()
}

/// Verify downloads are retained, reused across independent roots, and verified on every reuse.
/// 验证下载文件被保留、跨独立根复用，且每次复用均校验。
/// # Arguments
/// No parameters.
/// 无参数。
#[test]
fn retains_and_revalidates_installation_archive_without_sharing_system_roots() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path();
    let plugin = path.join("plugin");
    for directory in [
        "lib",
        "data",
        "default-data",
        "state/bootstrap",
        "tmp",
        "bin",
    ] {
        fs::create_dir_all(plugin.join(directory)).unwrap();
    }
    let preset = Path::new(env!("CARGO_MANIFEST_DIR")).join("presets/plugins/arch");
    for name in [
        "bootstrap.sh",
        "lib/common.sh",
        "lib/keyrings.sh",
        "default-data/mirrorlist",
        "default-data/pacman.conf.in",
    ] {
        fs::copy(preset.join(name), plugin.join(name)).unwrap();
    }
    let bytes = bootstrap_archive(None);
    let source = path.join("source.tar");
    fs::write(&source, &bytes).unwrap();
    let signature = path.join("source.sig");
    fs::write(&signature, format!("{:x}\n", Sha256::digest(&bytes))).unwrap();
    let keyring = path.join("trusted-keyring");
    fs::write(&keyring, "fixture").unwrap();
    let trace = path.join("trace");
    fs::write(&trace, "").unwrap();
    executable(
        &plugin.join("bin/curl"),
        r#"#!/bin/bash
set -euo pipefail
while (( $# )); do
    case "$1" in --output) output=$2; shift 2 ;; *) url=$1; shift ;; esac
done
printf 'download\n' >> "$BOOTSTRAP_TRACE"
if [[ $url == *.sig ]]; then cp "$BOOTSTRAP_SIGNATURE" "$output"; else cp "$BOOTSTRAP_SOURCE" "$output"; fi
"#,
    );
    executable(
        &plugin.join("bin/gpgv"),
        r#"#!/bin/bash
set -euo pipefail
printf 'verify\n' >> "$BOOTSTRAP_TRACE"
read -r expected < "${@: -2:1}"
actual=$(sha256sum "${@: -1}")
[[ ${actual%% *} == "$expected" ]]
"#,
    );
    let run = |name: &str, url: &str| {
        let root = path.join(name);
        fs::create_dir(&root).unwrap();
        Command::new("bash")
            .arg(plugin.join("bootstrap.sh"))
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    plugin.join("bin").display(),
                    std::env::var("PATH").unwrap()
                ),
            )
            .env("RSTAR_PLUGIN_ROOT", &plugin)
            .env("RSTAR_PLUGIN_TMP", plugin.join("tmp"))
            .env("RSTAR_ROOT", &root)
            .env("RSTAR_DISTRIBUTION_ARCHITECTURE", "x86_64")
            .env("RSTAR_DISTRIBUTION_KEYRING", &keyring)
            .env("RSTAR_DISTRIBUTION_BOOTSTRAPURL", url)
            .env(
                "RSTAR_DISTRIBUTION_MIRROR",
                "https://mirror.invalid/$repo/os/$arch",
            )
            .env("BOOTSTRAP_SOURCE", &source)
            .env("BOOTSTRAP_SIGNATURE", &signature)
            .env("BOOTSTRAP_TRACE", &trace)
            .output()
            .unwrap()
    };
    let url = "https://example.invalid/bootstrap.tar.zst";
    for name in ["desktop", "server"] {
        let output = run(name, url);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            fs::read_to_string(path.join(name).join("etc/pacman.d/mirrorlist")).unwrap(),
            "Server = https://mirror.invalid/$repo/os/$arch\n"
        );
        assert!(!path.join(name).join("pkglist.x86_64.txt").exists());
        assert!(!path.join(name).join("version").exists());
        assert_eq!(fs::read_dir(path.join(name)).unwrap().count(), 1);
    }
    assert_eq!(
        fs::read_to_string(&trace)
            .unwrap()
            .lines()
            .filter(|line| *line == "download")
            .count(),
        2
    );
    assert!(
        fs::read_to_string(&trace)
            .unwrap()
            .lines()
            .filter(|line| *line == "verify")
            .count()
            >= 2
    );
    assert!(
        run("new-source", "https://example.invalid/other.tar.zst")
            .status
            .success()
    );
    assert_eq!(
        fs::read_to_string(&trace)
            .unwrap()
            .lines()
            .filter(|line| *line == "download")
            .count(),
        4
    );
    let mut digest = Sha256::new();
    digest.update(url.as_bytes());
    digest.update([0]);
    digest.update(format!("{url}.sig").as_bytes());
    let cache = plugin
        .join("state/bootstrap")
        .join(format!("{:x}", digest.finalize()));
    fs::write(cache.join("rootfs.tar.zst"), "corrupt retained archive").unwrap();
    assert!(!run("damaged", url).status.success());
    assert_eq!(fs::read_dir(path.join("damaged")).unwrap().count(), 0);
    assert_eq!(
        fs::read_to_string(&trace)
            .unwrap()
            .lines()
            .filter(|line| *line == "download")
            .count(),
        4
    );
    for (name, extra_path) in [
        ("unexpected", "unexpected.txt"),
        ("nested-package-list", "pkglist.x86_64.txt/child"),
        ("nested-version", "version/child"),
        ("traversal", "root.x86_64/../escape"),
    ] {
        let bytes = bootstrap_archive(Some(extra_path));
        fs::write(&source, &bytes).unwrap();
        fs::write(&signature, format!("{:x}\n", Sha256::digest(&bytes))).unwrap();
        let output = run(name, &format!("https://example.invalid/{name}.tar.zst"));
        assert!(!output.status.success(), "accepted {extra_path}");
        assert_eq!(fs::read_dir(path.join(name)).unwrap().count(), 0);
    }
}

/// Stop the isolated signing agent before its temporary home is removed.
/// 在删除临时签名目录前停止隔离的签名代理。
struct SigningHome(tempfile::TempDir);

impl Drop for SigningHome {
    fn drop(&mut self) {
        let _ = Command::new("gpgconf")
            .arg("--homedir")
            .arg(self.0.path())
            .args(["--kill", "gpg-agent"])
            .output();
    }
}

/// Run real GnuPG without reading the user's configuration or keyring.
/// 使用真实 GnuPG，且不读取用户配置或密钥环。
fn gpg(home: &Path, arguments: &[&str]) -> Vec<u8> {
    let output = Command::new("gpg")
        .args(["--batch", "--no-options", "--homedir"])
        .arg(home)
        .args(arguments)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

/// Verify real signatures with all supported keyring formats and reject invalid inputs.
/// 使用所有受支持密钥环格式验证真实签名，并拒绝无效输入。
#[test]
fn verifies_armored_binary_and_keybox_keyrings_with_real_gpg() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path();
    let home = SigningHome(
        tempfile::Builder::new()
            .prefix("rstar-gpg-")
            .tempdir()
            .unwrap(),
    );
    let plugin = path.join("plugin with spaces");
    for directory in [
        "lib",
        "data",
        "default-data",
        "state/bootstrap",
        "tmp",
        "bin",
    ] {
        fs::create_dir_all(plugin.join(directory)).unwrap();
    }
    let preset = Path::new(env!("CARGO_MANIFEST_DIR")).join("presets/plugins/arch");
    for name in [
        "bootstrap.sh",
        "lib/common.sh",
        "lib/keyrings.sh",
        "default-data/mirrorlist",
        "default-data/pacman.conf.in",
    ] {
        fs::copy(preset.join(name), plugin.join(name)).unwrap();
    }
    let source = path.join("source.tar");
    fs::write(&source, bootstrap_archive(None)).unwrap();
    gpg(
        home.0.path(),
        &[
            "--pinentry-mode",
            "loopback",
            "--passphrase",
            "",
            "--quick-generate-key",
            "Bootstrap fixture <bootstrap@example.invalid>",
            "ed25519",
            "sign",
            "0",
        ],
    );
    let armored = path.join("armored keyring.gpg");
    let binary = path.join("binary keyring.gpg");
    let keybox = path.join("keybox ring.kbx");
    fs::write(&armored, gpg(home.0.path(), &["--armor", "--export"])).unwrap();
    fs::write(&binary, gpg(home.0.path(), &["--export"])).unwrap();
    fs::copy(home.0.path().join("pubring.kbx"), &keybox).unwrap();
    let signature = path.join("source.sig");
    gpg(
        home.0.path(),
        &[
            "--pinentry-mode",
            "loopback",
            "--passphrase",
            "",
            "--output",
            signature.to_str().unwrap(),
            "--detach-sign",
            source.to_str().unwrap(),
        ],
    );
    executable(
        &plugin.join("bin/curl"),
        r#"#!/bin/bash
set -euo pipefail
while (( $# )); do
    case "$1" in --output) output=$2; shift 2 ;; *) url=$1; shift ;; esac
done
if [[ $url == *.sig ]]; then cp "$BOOTSTRAP_SIGNATURE" "$output"; else cp "$BOOTSTRAP_SOURCE" "$output"; fi
"#,
    );
    let url = "https://example.invalid/bootstrap.tar.zst";
    let run = |name: &str, keyring: &Path| {
        let root = path.join(name);
        fs::create_dir(&root).unwrap();
        let output = Command::new("bash")
            .arg(plugin.join("bootstrap.sh"))
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    plugin.join("bin").display(),
                    std::env::var("PATH").unwrap()
                ),
            )
            .env("GNUPGHOME", home.0.path())
            .env("RSTAR_PLUGIN_ROOT", &plugin)
            .env("RSTAR_PLUGIN_TMP", plugin.join("tmp"))
            .env("RSTAR_ROOT", &root)
            .env("RSTAR_DISTRIBUTION_ARCHITECTURE", "x86_64")
            .env("RSTAR_DISTRIBUTION_KEYRING", keyring)
            .env("RSTAR_DISTRIBUTION_BOOTSTRAPURL", url)
            .env_remove("RSTAR_DISTRIBUTION_BOOTSTRAPSIGNATUREURL")
            .env("BOOTSTRAP_SOURCE", &source)
            .env("BOOTSTRAP_SIGNATURE", &signature)
            .output()
            .unwrap();
        assert_eq!(fs::read_dir(plugin.join("tmp")).unwrap().count(), 0);
        if !output.status.success() {
            assert_eq!(fs::read_dir(root).unwrap().count(), 0);
        }
        output
    };
    for (name, keyring) in [
        ("armored", &armored),
        ("binary", &binary),
        ("keybox", &keybox),
    ] {
        let output = run(name, keyring);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(path.join(name).join("etc/pacman.d/mirrorlist").is_file());
    }
    let malformed = path.join("malformed.gpg");
    fs::write(
        &malformed,
        "-----BEGIN PGP PUBLIC KEY BLOCK-----\ninvalid\n",
    )
    .unwrap();
    assert!(!run("malformed", &malformed).status.success());
    gpg(
        home.0.path(),
        &[
            "--pinentry-mode",
            "loopback",
            "--passphrase",
            "",
            "--quick-generate-key",
            "Wrong fixture <wrong@example.invalid>",
            "ed25519",
            "sign",
            "0",
        ],
    );
    let wrong = path.join("wrong.gpg");
    fs::write(
        &wrong,
        gpg(
            home.0.path(),
            &["--armor", "--export", "wrong@example.invalid"],
        ),
    )
    .unwrap();
    assert!(!run("wrong-key", &wrong).status.success());
    let mut digest = Sha256::new();
    digest.update(url.as_bytes());
    digest.update([0]);
    digest.update(format!("{url}.sig").as_bytes());
    let retained = plugin
        .join("state/bootstrap")
        .join(format!("{:x}", digest.finalize()));
    fs::write(retained.join("rootfs.tar.zst"), "tampered archive").unwrap();
    assert!(!run("tampered", &armored).status.success());
}

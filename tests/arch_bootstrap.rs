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
    for directory in ["lib", "data", "state/bootstrap", "tmp", "bin"] {
        fs::create_dir_all(plugin.join(directory)).unwrap();
    }
    let preset = Path::new(env!("CARGO_MANIFEST_DIR")).join("presets/plugins/arch");
    for name in ["bootstrap.sh", "lib/common.sh", "data/mirrorlist"] {
        fs::copy(preset.join(name), plugin.join(name)).unwrap();
    }
    let mut archive = tar::Builder::new(Vec::new());
    let contents = b"original mirror\n";
    let mut header = tar::Header::new_gnu();
    header.set_mode(0o644);
    header.set_size(contents.len() as u64);
    header.set_cksum();
    archive
        .append_data(
            &mut header,
            "root.x86_64/etc/pacman.d/mirrorlist",
            contents.as_slice(),
        )
        .unwrap();
    let bytes = archive.into_inner().unwrap();
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
}

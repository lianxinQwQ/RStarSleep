use super::*;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};

#[test]
fn guest_candidates_cannot_follow_links_or_open_special_files() {
    let temporary = tempfile::tempdir().unwrap();
    let root_path = temporary.path().join("root");
    fs::create_dir(&root_path).unwrap();
    let outside = temporary.path().join("outside");
    fs::write(&outside, "private").unwrap();
    symlink(&outside, root_path.join("link")).unwrap();
    symlink(temporary.path(), root_path.join("directory-link")).unwrap();
    fs::create_dir(root_path.join("directory")).unwrap();
    let fifo = CString::new(root_path.join("fifo").as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    let root = open_root(&root_path).unwrap();
    for path in [
        "/link",
        "/directory-link/outside",
        "/directory",
        "/fifo",
        "/missing",
        "/../outside",
    ] {
        assert!(open_file(&root, path, true).unwrap().is_none(), "{path}");
    }
    fs::write(root_path.join("regular"), "inside").unwrap();
    let mut file = open_file(&root, "/regular", true).unwrap().unwrap();
    // Replacing a directory entry after pinning cannot redirect the descriptor.
    fs::remove_file(root_path.join("regular")).unwrap();
    symlink(&outside, root_path.join("regular")).unwrap();
    use std::io::Write;
    file.write_all(b"updated").unwrap();
    assert_eq!(fs::read_to_string(outside).unwrap(), "private");
}

#[test]
fn index_is_bounded_and_rejects_links_and_special_files() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join(INDEX);
    assert!(read_index(&path).unwrap().is_none());
    let index = parse(b"pkg\t/usr/bin/a file\npkg\t/usr/bin/a file\n").unwrap();
    write_index(&path, &index).unwrap();
    assert_eq!(read_index(&path).unwrap().unwrap(), index);
    fs::write(&path, "invalid").unwrap();
    assert!(read_index(&path).is_err());
    fs::remove_file(&path).unwrap();
    symlink("/dev/null", &path).unwrap();
    assert!(read_index(&path).is_err());
}

/// Run explicitly on Btrfs to exercise real kernel validation, tail handling and CoW.
#[test]
#[ignore = "requires RSTAR_BTRFS_TEST_DIR pointing to a writable Btrfs directory"]
fn btrfs_kernel_shares_data_and_preserves_independent_files() {
    let directory = std::env::var_os("RSTAR_BTRFS_TEST_DIR").expect("set RSTAR_BTRFS_TEST_DIR");
    let temporary = tempfile::tempdir_in(directory).unwrap();
    let source_path = temporary.path().join("source");
    let target_path = temporary.path().join("target");
    // Cross the implementation's chunk boundary and end with an unaligned tail.
    let bytes = vec![b'a'; 16 * 1024 * 1024 + 123];
    fs::write(&source_path, &bytes).unwrap();
    fs::write(&target_path, &bytes).unwrap();
    fs::set_permissions(&source_path, fs::Permissions::from_mode(0o444)).unwrap();
    fs::set_permissions(&target_path, fs::Permissions::from_mode(0o600)).unwrap();
    let source = File::open(&source_path).unwrap();
    let mut target = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&target_path)
        .unwrap();
    let before = target.metadata().unwrap();
    assert_ne!(source.metadata().unwrap().ino(), before.ino());
    assert_eq!(
        share(&source, &target).unwrap(),
        DedupOutcome::Shared(bytes.len() as u64)
    );
    let after = target.metadata().unwrap();
    assert_eq!(
        (
            before.ino(),
            before.mode(),
            before.uid(),
            before.gid(),
            before.mtime(),
            before.mtime_nsec()
        ),
        (
            after.ino(),
            after.mode(),
            after.uid(),
            after.gid(),
            after.mtime(),
            after.mtime_nsec()
        )
    );
    assert_eq!(source.metadata().unwrap().mode() & 0o777, 0o444);
    use std::io::Write;
    target.write_all(b"changed").unwrap();
    assert_eq!(fs::read(&source_path).unwrap(), bytes);
    assert_eq!(share(&source, &target).unwrap(), DedupOutcome::Different);
}

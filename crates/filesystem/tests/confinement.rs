#![cfg(target_os = "linux")]

use std::{
    fs,
    io::{ErrorKind, Read, Write},
    os::unix::{ffi::OsStrExt, fs::symlink},
    path::Path,
    sync::Barrier,
    thread,
};

use arcdos_filesystem::{Entry, Kind, MAX_LIST_LIMIT, Root};
use rustix::{fs as sys_fs, io::Errno};
use tempfile::{tempdir, tempdir_in};

#[test]
fn rejects_absolute_paths_and_traversal() {
    let temp = tempdir().unwrap();
    let base = temp.path().join("data");
    fs::create_dir(&base).unwrap();
    fs::write(temp.path().join("secret"), b"outside").unwrap();
    let root = Root::open(&base).unwrap();

    for path in [
        "/secret",
        "../secret",
        "folder/../secret",
        "a//b",
        "a/",
        "./a",
        "a\\b",
        "a\0b",
    ] {
        assert_eq!(
            root.open_file(path).unwrap_err().kind(),
            ErrorKind::InvalidInput,
            "{path:?}"
        );
        assert_eq!(
            root.create_dir(path).unwrap_err().kind(),
            ErrorKind::InvalidInput,
            "{path:?}"
        );
    }
    assert_eq!(
        root.create_file("").unwrap_err().kind(),
        ErrorKind::InvalidInput
    );
    assert_eq!(
        root.list("", None, MAX_LIST_LIMIT + 1).unwrap_err().kind(),
        ErrorKind::InvalidInput
    );
    assert_eq!(fs::read(temp.path().join("secret")).unwrap(), b"outside");
    assert_eq!(root.metadata("").unwrap().kind, Kind::Directory);
}

#[test]
fn refuses_symlinks_hardlinks_and_special_entries() {
    let temp = tempdir().unwrap();
    let base = temp.path().join("data");
    let outside = temp.path().join("outside");
    fs::create_dir(&base).unwrap();
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("secret"), b"outside").unwrap();
    fs::write(base.join("visible"), b"inside").unwrap();
    fs::write(
        base.join(std::ffi::OsStr::from_bytes(b"invalid\xff")),
        b"inside",
    )
    .unwrap();
    symlink(&outside, base.join("escape")).unwrap();
    symlink(outside.join("secret"), base.join("link")).unwrap();
    symlink("visible", base.join("internal-link")).unwrap();
    fs::hard_link(outside.join("secret"), base.join("hardlink")).unwrap();
    sys_fs::mkfifoat(sys_fs::CWD, base.join("pipe"), sys_fs::Mode::RUSR).unwrap();
    let root = Root::open(&base).unwrap();

    for path in ["escape/secret", "link", "internal-link", "hardlink", "pipe"] {
        assert!(root.metadata(path).is_err(), "{path}");
        assert!(root.open_file(path).is_err(), "{path}");
        assert!(root.rename(path, &root, "moved").is_err(), "{path}");
    }
    assert!(Root::open(&base.join("escape")).is_err());
    assert!(root.list("escape", None, 5).is_err());
    assert!(root.create_file("escape/new").is_err());
    assert!(root.create_dir("escape/new").is_err());
    assert!(root.rename("visible", &root, "escape/new").is_err());
    assert!(root.remove_file("escape/secret").is_err());
    assert!(!outside.join("new").exists());
    assert_eq!(fs::read(outside.join("secret")).unwrap(), b"outside");

    let visible = root.list("", None, 5).unwrap();
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].name, "visible");
}

#[test]
fn anchored_root_survives_replacement_of_its_configured_path() {
    let temp = tempdir().unwrap();
    let initial = temp.path().join("data");
    let original = temp.path().join("original");
    let outside = temp.path().join("outside");
    fs::create_dir(&initial).unwrap();
    fs::create_dir(&outside).unwrap();
    fs::write(initial.join("file"), b"inside").unwrap();
    fs::write(outside.join("file"), b"outside").unwrap();
    let root = Root::open(&initial).unwrap();
    fs::rename(&initial, &original).unwrap();
    symlink(&outside, &initial).unwrap();

    let mut text = String::new();
    root.open_file("file")
        .unwrap()
        .read_to_string(&mut text)
        .unwrap();
    assert_eq!(text, "inside");
    root.create_dir("new").unwrap();
    assert!(original.join("new").is_dir());
    assert!(!outside.join("new").exists());
}

#[test]
fn concurrent_symlink_exchange_cannot_redirect_reads_or_writes() {
    let temp = tempdir().unwrap();
    let base = temp.path().join("data");
    let outside = temp.path().join("outside");
    let inside = base.join("subdir");
    let swap = base.join("swap");
    fs::create_dir(&base).unwrap();
    fs::create_dir(&inside).unwrap();
    fs::create_dir(&outside).unwrap();
    fs::write(inside.join("file"), b"inside").unwrap();
    fs::write(outside.join("file"), b"outside").unwrap();
    symlink(&outside, &swap).unwrap();
    let root = Root::open(&base).unwrap();
    let start = Barrier::new(2);

    thread::scope(|scope| {
        scope.spawn(|| {
            start.wait();
            for _ in 0..1000 {
                sys_fs::renameat_with(
                    sys_fs::CWD,
                    &inside,
                    sys_fs::CWD,
                    &swap,
                    sys_fs::RenameFlags::EXCHANGE,
                )
                .unwrap();
            }
        });
        start.wait();
        for index in 0..500 {
            if let Ok(mut file) = root.open_file("subdir/file") {
                let mut content = String::new();
                file.read_to_string(&mut content).unwrap();
                assert_eq!(content, "inside");
            }
            let _ = root.create_dir(&format!("subdir/new-{index}"));
        }
    });

    assert_eq!(fs::read(outside.join("file")).unwrap(), b"outside");
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 1);
}

#[test]
fn directory_pagination_is_bounded_and_name_ordered() {
    let temp = tempdir().unwrap();
    let root = Root::open(temp.path()).unwrap();
    for index in (0..3000).rev() {
        root.create_file(&format!("item-{index:04}")).unwrap();
    }
    root.create_file("ζήτω-😀").unwrap();

    let page = root.list("", None, 37).unwrap();
    assert_eq!(page.len(), 37);
    assert_eq!(page[0].name, "item-0000");
    assert_eq!(page.last().unwrap().name, "item-0036");
    let next = root.list("", Some("item-0036"), 37).unwrap();
    assert_eq!(next[0].name, "item-0037");
    assert_eq!(next.last().unwrap().name, "item-0073");
    let final_page = root.list("", Some("item-2998"), 37).unwrap();
    assert_eq!(
        final_page
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        ["item-2999", "ζήτω-😀"]
    );
    assert!(root.list("", Some("ζήτω-😀"), 37).unwrap().is_empty());
}

#[test]
fn copy_move_trash_and_restore_without_clobbering() {
    let temp = tempdir().unwrap();
    let files = temp.path().join("files");
    let staging = temp.path().join("state");
    fs::create_dir(&files).unwrap();
    fs::create_dir(&staging).unwrap();
    let root = Root::open(&files).unwrap();
    let stage = Root::open(&staging).unwrap();
    root.create_dir("folder").unwrap();
    root.create_file("folder/original")
        .unwrap()
        .write_all(b"original")
        .unwrap();
    root.create_file("folder/existing")
        .unwrap()
        .write_all(b"existing")
        .unwrap();

    let old = root.metadata("folder/original").unwrap();
    let conflict = root
        .rename("folder/original", &root, "folder/existing")
        .unwrap_err();
    assert_eq!(conflict.kind(), ErrorKind::AlreadyExists);
    assert_eq!(
        fs::read(files.join("folder/existing")).unwrap(),
        b"existing"
    );
    assert_eq!(
        fs::read(files.join("folder/original")).unwrap(),
        b"original"
    );

    root.copy_file("folder/original", &stage, "copy-stage")
        .unwrap();
    assert_eq!(
        root.copy_file("folder/original", &stage, "copy-stage")
            .unwrap_err()
            .kind(),
        ErrorKind::AlreadyExists
    );
    stage.rename("copy-stage", &root, "copy").unwrap();
    assert_eq!(fs::read(files.join("copy")).unwrap(), b"original");
    assert_ne!(root.metadata("copy").unwrap().ino, old.ino);

    root.rename("folder", &stage, "trash-id").unwrap();
    assert!(!files.join("folder").exists());
    stage.rename("trash-id", &root, "restored").unwrap();
    assert_eq!(root.metadata("restored/original").unwrap().ino, old.ino);
    let open = root.open_file("restored/original").unwrap();
    assert_eq!(
        root.metadata("restored/original").unwrap(),
        Entry::from_file(&open).unwrap()
    );
    root.sync_dir("restored").unwrap();
    root.sync_dir("").unwrap();
    stage.sync_dir("").unwrap();

    stage.create_dir("aborted").unwrap();
    stage.create_file("aborted/partial").unwrap();
    assert!(stage.remove_dir("aborted").is_err());
    stage.remove_file("aborted/partial").unwrap();
    stage.remove_dir("aborted").unwrap();
}

#[test]
fn cross_filesystem_rename_preserves_source_and_destination() {
    let root_dir = tempdir().unwrap();
    let Ok(other_dir) = tempdir_in("/dev/shm") else {
        return;
    };
    let root = Root::open(root_dir.path()).unwrap();
    let other = Root::open(other_dir.path()).unwrap();
    if root.metadata("").unwrap().dev == other.metadata("").unwrap().dev {
        return;
    }
    root.create_file("original")
        .unwrap()
        .write_all(b"original")
        .unwrap();

    let error = root.rename("original", &other, "new").unwrap_err();
    assert_eq!(error.raw_os_error(), Some(Errno::XDEV.raw_os_error()));
    assert_eq!(
        fs::read(root_dir.path().join("original")).unwrap(),
        b"original"
    );
    assert!(!other_dir.path().join("new").exists());
}

#[test]
#[ignore = "requires a dedicated 64 KiB tmpfs at /arcdos-fs-test/staging"]
fn nested_mount_is_excluded_and_exhausted_staging_removes_partial_copy() {
    let boundary = Path::new("/arcdos-fs-test");
    let mount = boundary.join("staging");
    let stats = sys_fs::statfs(&mount).unwrap();
    assert_eq!(stats.f_type, 0x0102_1994);
    assert_eq!(stats.f_blocks as u128 * stats.f_bsize as u128, 65_536);

    let root = Root::open(boundary).unwrap();
    let error = root.metadata("staging").unwrap_err();
    assert_eq!(error.raw_os_error(), Some(Errno::XDEV.raw_os_error()));
    assert!(
        !root
            .list("", None, 5)
            .unwrap()
            .iter()
            .any(|entry| entry.name == "staging")
    );
    assert!(root.create_file("staging/forbidden").is_err());

    let files = tempdir().unwrap();
    let staging = tempdir_in(mount).unwrap();
    let root = Root::open(files.path()).unwrap();
    let stage = Root::open(staging.path()).unwrap();
    let data = vec![0xa7; 131_072];
    root.create_file("original")
        .unwrap()
        .write_all(&data)
        .unwrap();

    let error = root.copy_file("original", &stage, "partial").unwrap_err();
    assert_eq!(error.raw_os_error(), Some(Errno::NOSPC.raw_os_error()));
    assert!(!staging.path().join("partial").exists());
    assert_eq!(fs::read(files.path().join("original")).unwrap(), data);
}

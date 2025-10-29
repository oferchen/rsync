use super::*;
use std::fs;
use std::path::{Path, PathBuf};

fn collect_relative_paths(walker: Walker) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for entry in walker {
        let entry = entry.expect("walker entry");
        if entry.is_root() {
            continue;
        }
        paths.push(entry.relative_path().to_path_buf());
    }
    paths
}

#[test]
fn walk_errors_when_root_missing() {
    let builder = WalkBuilder::new("/nonexistent/path/for/walker");
    let error = match builder.build() {
        Ok(_) => panic!("missing root should fail"),
        Err(error) => error,
    };
    assert!(matches!(error.kind(), WalkErrorKind::RootMetadata { .. }));
    assert_eq!(error.path(), Path::new("/nonexistent/path/for/walker"));
    assert_eq!(
        error.kind().path(),
        Path::new("/nonexistent/path/for/walker")
    );
}

#[test]
fn walk_single_file_emits_root_entry() {
    let temp = tempfile::tempdir().expect("tempdir");
    let file = temp.path().join("file.txt");
    fs::write(&file, b"contents").expect("write");

    let mut walker = WalkBuilder::new(&file).build().expect("build walker");
    let entry = walker.next().expect("entry").expect("entry ok");
    assert!(entry.is_root());
    assert!(entry.relative_path().as_os_str().is_empty());
    assert_eq!(entry.full_path(), file);
    assert!(walker.next().is_none());
}

#[test]
fn walk_directory_yields_deterministic_order() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("root");
    fs::create_dir(&root).expect("create root");
    let dir_a = root.join("a");
    let dir_b = root.join("b");
    let file_c = root.join("c.txt");
    fs::create_dir(&dir_a).expect("dir a");
    fs::create_dir(&dir_b).expect("dir b");
    fs::write(dir_a.join("inner.txt"), b"data").expect("write inner");
    fs::write(&file_c, b"data").expect("write file");

    let walker = WalkBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);
    assert_eq!(
        paths,
        vec![
            PathBuf::from("a"),
            PathBuf::from("a/inner.txt"),
            PathBuf::from("b"),
            PathBuf::from("c.txt"),
        ]
    );
}

#[cfg(unix)]
#[test]
fn walk_does_not_follow_symlink_by_default() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("root");
    let target = temp.path().join("target");
    fs::create_dir(&root).expect("create root");
    fs::create_dir(&target).expect("create target");
    fs::write(target.join("inner.txt"), b"data").expect("write inner");
    symlink(&target, root.join("link")).expect("create symlink");

    let walker = WalkBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);
    assert_eq!(paths, vec![PathBuf::from("link")]);
}

#[cfg(unix)]
#[test]
fn walk_follows_symlink_when_enabled() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("root");
    let target = temp.path().join("target");
    fs::create_dir(&root).expect("create root");
    fs::create_dir(&target).expect("create target");
    fs::write(target.join("inner.txt"), b"data").expect("write inner");
    symlink(&target, root.join("link")).expect("create symlink");

    let walker = WalkBuilder::new(&root)
        .follow_symlinks(true)
        .build()
        .expect("build walker");
    let paths = collect_relative_paths(walker);
    assert_eq!(
        paths,
        vec![PathBuf::from("link"), PathBuf::from("link/inner.txt")]
    );
}

#[cfg(unix)]
#[test]
fn walk_root_symlink_followed_when_enabled() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().expect("tempdir");
    let target = temp.path().join("target");
    fs::create_dir(&target).expect("create target");
    fs::write(target.join("file.txt"), b"data").expect("write file");

    let link = temp.path().join("link");
    symlink(&target, &link).expect("create symlink");

    let walker = WalkBuilder::new(&link)
        .follow_symlinks(true)
        .build()
        .expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths, vec![PathBuf::from("file.txt")]);
}

#[cfg(unix)]
#[test]
fn walk_root_symlink_preserves_full_paths() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().expect("tempdir");
    let target = temp.path().join("target");
    fs::create_dir(&target).expect("create target");
    let file = target.join("file.txt");
    fs::write(&file, b"data").expect("write file");

    let link = temp.path().join("link");
    symlink(&target, &link).expect("create symlink");

    let mut walker = WalkBuilder::new(&link)
        .follow_symlinks(true)
        .build()
        .expect("build walker");

    let root = walker.next().expect("root entry").expect("root ok");
    assert!(root.is_root());
    assert!(root.metadata().file_type().is_symlink());
    assert_eq!(root.full_path(), link.as_path());
    assert!(root.relative_path().as_os_str().is_empty());

    let child = walker.next().expect("child entry").expect("child ok");
    assert_eq!(child.relative_path(), std::path::Path::new("file.txt"));
    assert_eq!(child.full_path(), link.join("file.txt"));
    assert!(child.metadata().is_file());

    assert!(walker.next().is_none());
}

#[cfg(unix)]
#[test]
fn walk_root_symlink_not_followed_by_default() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().expect("tempdir");
    let target = temp.path().join("target");
    fs::create_dir(&target).expect("create target");
    fs::write(target.join("file.txt"), b"data").expect("write file");

    let link = temp.path().join("link");
    symlink(&target, &link).expect("create symlink");

    let mut walker = WalkBuilder::new(&link).build().expect("build walker");
    let root = walker.next().expect("root entry").expect("root ok");
    assert!(root.is_root());
    assert!(root.metadata().file_type().is_symlink());
    assert!(walker.next().is_none());
}

#[cfg(unix)]
#[test]
fn walk_detects_symlink_cycles() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("root");
    fs::create_dir(&root).expect("create root");
    let _ = symlink(&root, root.join("self"));

    let walker = WalkBuilder::new(&root)
        .follow_symlinks(true)
        .build()
        .expect("build walker");
    let paths = collect_relative_paths(walker);
    assert_eq!(paths, vec![PathBuf::from("self")]);
}

#[test]
fn walk_entry_file_name_matches_tail_component() {
    use std::ffi::OsStr;

    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("root");
    let nested_dir = root.join("nested");
    let nested_file = nested_dir.join("file.txt");
    fs::create_dir_all(&nested_dir).expect("create nested");
    fs::write(&nested_file, b"data").expect("write nested file");

    let mut walker = WalkBuilder::new(&root).build().expect("build walker");
    let root_entry = walker.next().expect("root entry").expect("root ok");
    assert!(root_entry.is_root());
    assert!(root_entry.file_name().is_none());

    let dir_entry = walker.next().expect("dir entry").expect("dir ok");
    assert_eq!(dir_entry.file_name(), Some(OsStr::new("nested")));

    let file_entry = walker.next().expect("file entry").expect("file ok");
    assert_eq!(file_entry.file_name(), Some(OsStr::new("file.txt")));
}

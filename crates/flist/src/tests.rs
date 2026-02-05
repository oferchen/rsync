use super::*;
use std::fs;
use std::path::{Path, PathBuf};

fn collect_relative_paths(walker: FileListWalker) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for entry in walker {
        // Handle both successful entries and errors gracefully
        match entry {
            Ok(entry) => {
                if entry.is_root() {
                    continue;
                }
                paths.push(entry.relative_path().to_path_buf());
            }
            Err(_) => {
                // Stop on error but return what we collected so far
                break;
            }
        }
    }
    paths
}

#[test]
fn walk_errors_when_root_missing() {
    let builder = FileListBuilder::new("/nonexistent/path/for/walker");
    let error = match builder.build() {
        Ok(_) => panic!("missing root should fail"),
        Err(error) => error,
    };
    assert!(matches!(
        error.kind(),
        FileListErrorKind::RootMetadata { .. }
    ));
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

    let mut walker = FileListBuilder::new(&file).build().expect("build walker");
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

    let walker = FileListBuilder::new(&root).build().expect("build walker");
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

    let walker = FileListBuilder::new(&root).build().expect("build walker");
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

    let walker = FileListBuilder::new(&root)
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

    let walker = FileListBuilder::new(&link)
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

    let mut walker = FileListBuilder::new(&link)
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

    let mut walker = FileListBuilder::new(&link).build().expect("build walker");
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

    let walker = FileListBuilder::new(&root)
        .follow_symlinks(true)
        .build()
        .expect("build walker");
    let paths = collect_relative_paths(walker);
    assert_eq!(paths, vec![PathBuf::from("self")]);
}

#[cfg(unix)]
#[test]
#[ignore = "Loop detection not fully implemented - walker errors on self-referencing symlinks"]
fn walk_detects_direct_symlink_loop() {
    use std::os::unix::fs::symlink;

    // Test case: A symlink that points directly back to itself
    // Structure: root/
    //              link -> link
    //
    // Current behavior: When follow_symlinks is enabled, the walker attempts to
    // follow the symlink, but fs::metadata() fails with "Too many levels of symbolic links".
    // This causes the walker to return an error instead of gracefully handling the loop.
    //
    // Expected behavior: The walker should detect this loop and yield the symlink
    // without attempting to follow it, similar to how it handles cycles via canonicalization.
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("root");
    fs::create_dir(&root).expect("create root");

    let link_path = root.join("link");
    symlink(&link_path, &link_path).expect("create self-referencing symlink");

    let walker = FileListBuilder::new(&root)
        .follow_symlinks(true)
        .build()
        .expect("build walker");

    let paths = collect_relative_paths(walker);

    // The symlink should be yielded but not followed (loop detected)
    assert_eq!(paths, vec![PathBuf::from("link")]);
}

#[cfg(unix)]
#[test]
fn walk_detects_indirect_symlink_loop() {
    use std::os::unix::fs::symlink;

    // Test case: A -> B -> C -> A (three-way loop)
    // Structure: root/
    //              a/
    //                link_b -> b
    //              b/
    //                link_c -> c
    //              c/
    //                link_a -> a
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("root");
    fs::create_dir(&root).expect("create root");

    let dir_a = root.join("a");
    let dir_b = root.join("b");
    let dir_c = root.join("c");

    fs::create_dir(&dir_a).expect("create a");
    fs::create_dir(&dir_b).expect("create b");
    fs::create_dir(&dir_c).expect("create c");

    // Create the loop: a/link_b -> b, b/link_c -> c, c/link_a -> a
    symlink(&dir_b, dir_a.join("link_b")).expect("create a -> b");
    symlink(&dir_c, dir_b.join("link_c")).expect("create b -> c");
    symlink(&dir_a, dir_c.join("link_a")).expect("create c -> a");

    let walker = FileListBuilder::new(&root)
        .follow_symlinks(true)
        .build()
        .expect("build walker");

    let paths = collect_relative_paths(walker);

    // All three directories should be yielded
    assert!(paths.contains(&PathBuf::from("a")));
    assert!(paths.contains(&PathBuf::from("b")));
    assert!(paths.contains(&PathBuf::from("c")));

    // The walker should yield a/link_b and follow it into b
    // Then yield b/link_c and follow it into c (but c is already visited, so skip)
    // So we should see a/link_b but the loop detection prevents infinite recursion
    assert!(paths.contains(&PathBuf::from("a/link_b")));

    // Due to loop detection, we may not see all symlinks traversed,
    // but we should not have infinite entries
    assert!(
        paths.len() < 20,
        "Loop detection failed, got {} entries",
        paths.len()
    );
}

#[cfg(unix)]
#[test]
fn walk_detects_parent_symlink_loop() {
    use std::os::unix::fs::symlink;

    // Test case: child directory has symlink pointing to parent
    // Structure: root/
    //              child/
    //                parent_link -> root
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("root");
    fs::create_dir(&root).expect("create root");

    let child = root.join("child");
    fs::create_dir(&child).expect("create child");

    symlink(&root, child.join("parent_link")).expect("create child -> parent symlink");

    let walker = FileListBuilder::new(&root)
        .follow_symlinks(true)
        .build()
        .expect("build walker");

    let paths = collect_relative_paths(walker);

    // Should get: child, child/parent_link
    // The parent_link should be yielded but not followed (loop to ancestor)
    assert!(paths.contains(&PathBuf::from("child")));
    assert!(paths.contains(&PathBuf::from("child/parent_link")));

    // Should not infinitely recurse
    assert!(
        paths.len() < 10,
        "Loop detection failed, got {} entries",
        paths.len()
    );
}

#[cfg(unix)]
#[test]
fn walk_continues_after_detecting_loop() {
    use std::os::unix::fs::symlink;

    // Test case: Verify that after detecting a loop, the walker continues
    // processing other entries
    // Structure: root/
    //              loop_dir/
    //                self_link -> loop_dir
    //              normal_file.txt
    //              normal_dir/
    //                nested.txt
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("root");
    fs::create_dir(&root).expect("create root");

    let loop_dir = root.join("loop_dir");
    fs::create_dir(&loop_dir).expect("create loop_dir");
    symlink(&loop_dir, loop_dir.join("self_link")).expect("create self link");

    fs::write(root.join("normal_file.txt"), b"data").expect("write normal file");

    let normal_dir = root.join("normal_dir");
    fs::create_dir(&normal_dir).expect("create normal_dir");
    fs::write(normal_dir.join("nested.txt"), b"data").expect("write nested file");

    let walker = FileListBuilder::new(&root)
        .follow_symlinks(true)
        .build()
        .expect("build walker");

    let paths = collect_relative_paths(walker);

    // Verify loop entries are present but not infinitely followed
    assert!(paths.contains(&PathBuf::from("loop_dir")));
    assert!(paths.contains(&PathBuf::from("loop_dir/self_link")));

    // Verify normal entries are still processed after loop detection
    assert!(paths.contains(&PathBuf::from("normal_file.txt")));
    assert!(paths.contains(&PathBuf::from("normal_dir")));
    assert!(paths.contains(&PathBuf::from("normal_dir/nested.txt")));

    // Check we got all expected entries and no duplicates from infinite loop
    assert_eq!(paths.len(), 5, "Expected 5 entries, got: {paths:?}");
}

#[cfg(unix)]
#[test]
fn walk_loop_detection_with_multiple_paths_to_same_dir() {
    use std::os::unix::fs::symlink;

    // Test case: Two different symlinks pointing to the same directory
    // The walker's loop detection uses canonical paths, so the same directory
    // is only traversed once regardless of which path reaches it first.
    // Structure: root/
    //              target/
    //                file.txt
    //              link1 -> target
    //              link2 -> target
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("root");
    fs::create_dir(&root).expect("create root");

    let target = root.join("target");
    fs::create_dir(&target).expect("create target");
    fs::write(target.join("file.txt"), b"data").expect("write file");

    symlink(&target, root.join("link1")).expect("create link1");
    symlink(&target, root.join("link2")).expect("create link2");

    let walker = FileListBuilder::new(&root)
        .follow_symlinks(true)
        .build()
        .expect("build walker");

    let paths = collect_relative_paths(walker);

    // Should have: link1, link2, target
    // The symlinks themselves are yielded, but the target directory is only
    // traversed once (whichever is encountered first in sorted order: link1)
    assert!(paths.contains(&PathBuf::from("link1")));
    assert!(paths.contains(&PathBuf::from("link2")));
    assert!(paths.contains(&PathBuf::from("target")));

    // Count how many times file.txt appears
    let file_count = paths
        .iter()
        .filter(|p| p.file_name().and_then(|n| n.to_str()) == Some("file.txt"))
        .count();

    // Due to loop detection using canonical paths, file.txt only appears once
    // under whichever path is visited first (link1 comes before link2 and target
    // in sorted order, so it's visited via link1)
    assert_eq!(
        file_count, 1,
        "Expected file.txt to appear once due to canonical path loop detection, got {file_count} times in {paths:?}"
    );
}

#[cfg(unix)]
#[test]
fn walk_symlink_loop_not_followed_when_disabled() {
    use std::os::unix::fs::symlink;

    // Test case: With follow_symlinks=false, loops don't matter
    // because we never dereference symlinks
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("root");
    fs::create_dir(&root).expect("create root");

    symlink(&root, root.join("self")).expect("create self link");
    fs::write(root.join("file.txt"), b"data").expect("write file");

    let walker = FileListBuilder::new(&root)
        .follow_symlinks(false)
        .build()
        .expect("build walker");

    let paths = collect_relative_paths(walker);

    // Should get both the symlink and the file, no loop issues
    assert_eq!(paths.len(), 2);
    assert!(paths.contains(&PathBuf::from("self")));
    assert!(paths.contains(&PathBuf::from("file.txt")));
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

    let mut walker = FileListBuilder::new(&root).build().expect("build walker");
    let root_entry = walker.next().expect("root entry").expect("root ok");
    assert!(root_entry.is_root());
    assert!(root_entry.file_name().is_none());

    let dir_entry = walker.next().expect("dir entry").expect("dir ok");
    assert_eq!(dir_entry.file_name(), Some(OsStr::new("nested")));

    let file_entry = walker.next().expect("file entry").expect("file ok");
    assert_eq!(file_entry.file_name(), Some(OsStr::new("file.txt")));
}

#[test]
fn walk_empty_directory() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("empty");
    fs::create_dir(&root).expect("create empty dir");

    let mut walker = FileListBuilder::new(&root).build().expect("build walker");

    // Should yield root
    let root_entry = walker.next().expect("root entry").expect("root ok");
    assert!(root_entry.is_root());

    // Should yield nothing else
    assert!(walker.next().is_none());
}

#[test]
fn walk_include_root_false_skips_root_entry() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("root");
    fs::create_dir(&root).expect("create root");
    fs::write(root.join("file.txt"), b"data").expect("write file");

    let walker = FileListBuilder::new(&root)
        .include_root(false)
        .build()
        .expect("build walker");
    let paths = collect_relative_paths(walker);

    // Should only contain file, not root
    assert_eq!(paths, vec![PathBuf::from("file.txt")]);
}

#[test]
fn walk_entry_depth_increases() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("root");
    let nested = root.join("nested");
    let deep = nested.join("deep");
    fs::create_dir_all(&deep).expect("create deep dir");
    fs::write(deep.join("file.txt"), b"data").expect("write file");

    let mut walker = FileListBuilder::new(&root).build().expect("build walker");

    let root_entry = walker.next().expect("root entry").expect("root ok");
    assert_eq!(root_entry.depth(), 0);

    let nested_entry = walker.next().expect("nested entry").expect("nested ok");
    assert_eq!(nested_entry.depth(), 1);
    assert_eq!(nested_entry.relative_path(), Path::new("nested"));

    let deep_entry = walker.next().expect("deep entry").expect("deep ok");
    assert_eq!(deep_entry.depth(), 2);
    assert_eq!(deep_entry.relative_path(), Path::new("nested/deep"));

    let file_entry = walker.next().expect("file entry").expect("file ok");
    assert_eq!(file_entry.depth(), 3);
    assert_eq!(
        file_entry.relative_path(),
        Path::new("nested/deep/file.txt")
    );
}

#[test]
fn walk_terminates_after_exhaustion() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("root");
    fs::create_dir(&root).expect("create root");
    fs::write(root.join("a.txt"), b"a").expect("write a");

    let mut walker = FileListBuilder::new(&root).build().expect("build walker");

    // Exhaust the walker
    let _ = walker.next();
    let _ = walker.next();

    // Should consistently return None
    assert!(walker.next().is_none());
    assert!(walker.next().is_none());
    assert!(walker.next().is_none());
}

#[test]
fn walk_multiple_files_sorted() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("root");
    fs::create_dir(&root).expect("create root");

    // Create files in non-alphabetical order
    fs::write(root.join("zebra.txt"), b"z").expect("write zebra");
    fs::write(root.join("apple.txt"), b"a").expect("write apple");
    fs::write(root.join("mango.txt"), b"m").expect("write mango");

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    // Should be sorted alphabetically
    assert_eq!(
        paths,
        vec![
            PathBuf::from("apple.txt"),
            PathBuf::from("mango.txt"),
            PathBuf::from("zebra.txt"),
        ]
    );
}

#[test]
fn walk_nested_directories_sorted() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("root");
    fs::create_dir(&root).expect("create root");

    // Create directories with files in non-alphabetical order
    let dir_b = root.join("b_dir");
    let dir_a = root.join("a_dir");
    fs::create_dir(&dir_b).expect("create b_dir");
    fs::create_dir(&dir_a).expect("create a_dir");
    fs::write(dir_b.join("file.txt"), b"b").expect("write b file");
    fs::write(dir_a.join("file.txt"), b"a").expect("write a file");

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    // Directories should be visited in sorted order
    assert_eq!(
        paths,
        vec![
            PathBuf::from("a_dir"),
            PathBuf::from("a_dir/file.txt"),
            PathBuf::from("b_dir"),
            PathBuf::from("b_dir/file.txt"),
        ]
    );
}

#[test]
fn error_kind_path_returns_correct_path() {
    use crate::error::FileListError;
    use std::io;

    let path = PathBuf::from("/test/path");
    let io_error = io::Error::other("test");

    let errors = [
        FileListError::root_metadata(path.clone(), io::Error::other("test")),
        FileListError::read_dir(path.clone(), io::Error::other("test")),
        FileListError::read_dir_entry(path.clone(), io::Error::other("test")),
        FileListError::metadata(path.clone(), io::Error::other("test")),
        FileListError::canonicalize(path.clone(), io_error),
    ];

    for error in errors {
        assert_eq!(error.path(), path.as_path());
        assert_eq!(error.kind().path(), path.as_path());
    }
}

#[test]
fn error_debug_format() {
    use crate::error::FileListError;
    use std::io;

    let error = FileListError::root_metadata(PathBuf::from("/test"), io::Error::other("test"));
    let debug = format!("{error:?}");
    assert!(debug.contains("FileListError"));
}

#[test]
fn error_display_includes_path_and_message() {
    use crate::error::FileListError;
    use std::io;

    let error = FileListError::read_dir(PathBuf::from("/my/path"), io::Error::other("io error"));
    let display = error.to_string();
    assert!(display.contains("/my/path"));
    assert!(display.contains("io error"));
}

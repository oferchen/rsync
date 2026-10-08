use super::*;
use std::io::Read;
use std::os::unix::fs::{MetadataExt, symlink};

const SECRET: &str = "outside-secret";

fn read(file: File) -> String {
    let mut text = String::new();
    let mut file = file;
    file.read_to_string(&mut text).expect("read");
    text
}

/// `base/src/sub/f` (in-tree) and `base/outside/f` (the secret).
fn tree() -> (tempfile::TempDir, PathBuf) {
    let tmp = test_support::create_tempdir();
    let base = tmp.path().to_path_buf();
    std::fs::create_dir_all(base.join("src/sub")).expect("mkdir sub");
    std::fs::create_dir_all(base.join("outside")).expect("mkdir outside");
    std::fs::write(base.join("src/sub/f"), "inside").expect("write inside");
    std::fs::write(base.join("outside/f"), SECRET).expect("write secret");
    (tmp, base)
}

fn remember_dir(roots: &SourceRoots, dir: &Path) {
    let meta = std::fs::metadata(dir).expect("stat root");
    roots
        .remember_operand(dir, true, meta.dev(), meta.ino())
        .expect("remember");
}

#[test]
fn reads_a_file_beneath_its_root() {
    let (_tmp, base) = tree();
    let roots = SourceRoots::new();
    remember_dir(&roots, &base.join("src"));
    let file = roots.open(&base.join("src/sub/f"), false).expect("matched");
    assert_eq!(read(file.expect("open")), "inside");
}

#[test]
fn a_parent_swapped_for_an_escaping_symlink_is_refused() {
    let (_tmp, base) = tree();
    let roots = SourceRoots::new();
    remember_dir(&roots, &base.join("src"));
    std::fs::rename(base.join("src/sub"), base.join("src/.realsub")).expect("move sub");
    symlink("../outside", base.join("src/sub")).expect("plant symlink");
    let opened = roots.open(&base.join("src/sub/f"), false).expect("matched");
    assert!(opened.is_err(), "escape through src/sub must be refused");
}

#[test]
fn a_root_replaced_after_it_was_recorded_is_refused_with_eloop() {
    let (_tmp, base) = tree();
    let roots = SourceRoots::new();
    remember_dir(&roots, &base.join("src"));
    std::fs::create_dir(base.join("outside/sub")).expect("mkdir decoy");
    std::fs::write(base.join("outside/sub/f"), SECRET).expect("write decoy");
    std::fs::rename(base.join("src"), base.join("src.real")).expect("move root");
    symlink(base.join("outside"), base.join("src")).expect("plant root symlink");
    let error = roots
        .open(&base.join("src/sub/f"), false)
        .expect("matched")
        .expect_err("swapped root must be refused");
    assert_eq!(error.raw_os_error(), Some(libc::ELOOP));
}

#[test]
fn an_in_tree_directory_symlink_is_followed() {
    let (_tmp, base) = tree();
    symlink("sub", base.join("src/alias")).expect("in-tree symlink");
    let roots = SourceRoots::new();
    remember_dir(&roots, &base.join("src"));
    let file = roots
        .open(&base.join("src/alias/f"), false)
        .expect("matched");
    assert_eq!(read(file.expect("open")), "inside");
}

#[test]
fn a_symlinked_leaf_is_refused() {
    let (_tmp, base) = tree();
    symlink(base.join("outside/f"), base.join("src/leaf")).expect("leaf symlink");
    let roots = SourceRoots::new();
    remember_dir(&roots, &base.join("src"));
    let error = roots
        .open(&base.join("src/leaf"), false)
        .expect("matched")
        .expect_err("leaf symlink must be refused");
    assert_eq!(error.raw_os_error(), Some(libc::ELOOP));
}

#[test]
fn a_path_outside_every_root_is_not_matched() {
    let (_tmp, base) = tree();
    let roots = SourceRoots::new();
    remember_dir(&roots, &base.join("src"));
    assert!(roots.open(&base.join("outside/f"), false).is_none());
    assert!(
        SourceRoots::new()
            .open(&base.join("src/sub/f"), false)
            .is_none()
    );
}

#[test]
fn a_file_operand_pins_its_parent_directory() {
    let (_tmp, base) = tree();
    let file = base.join("src/sub/f");
    let meta = std::fs::symlink_metadata(&file).expect("stat file");
    let roots = SourceRoots::new();
    roots
        .remember_operand(&file, false, meta.dev(), meta.ino())
        .expect("remember");
    std::fs::rename(base.join("src/sub"), base.join("src/.realsub")).expect("move sub");
    symlink("../outside", base.join("src/sub")).expect("plant symlink");
    let error = roots
        .open(&file, false)
        .expect("matched")
        .expect_err("swapped parent root must be refused");
    assert_eq!(error.raw_os_error(), Some(libc::ELOOP));
}

#[test]
fn the_longest_matching_root_anchors_the_open() {
    let (_tmp, base) = tree();
    let roots = SourceRoots::new();
    remember_dir(&roots, &base.join("src"));
    remember_dir(&roots, &base.join("src/sub"));
    // Replacing src/sub defeats only a root that is src/sub itself: the
    // longer root must be the one consulted.
    std::fs::rename(base.join("src/sub"), base.join("src/.realsub")).expect("move sub");
    std::fs::create_dir(base.join("src/sub")).expect("recreate sub");
    std::fs::write(base.join("src/sub/f"), "replaced").expect("write replaced");
    let error = roots
        .open(&base.join("src/sub/f"), false)
        .expect("matched")
        .expect_err("replaced inner root must be refused");
    assert_eq!(error.raw_os_error(), Some(libc::ELOOP));
}

#[test]
fn dot_and_dotdot_components_match_the_cleaned_root() {
    let (_tmp, base) = tree();
    let roots = SourceRoots::new();
    remember_dir(&roots, &base.join("src/./sub/.."));
    let file = roots
        .open(&base.join("outside/../src/./sub/f"), false)
        .expect("matched");
    assert_eq!(read(file.expect("open")), "inside");
}

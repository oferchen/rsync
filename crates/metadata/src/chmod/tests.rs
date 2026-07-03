use super::ChmodModifiers;

#[test]
fn parse_symbolic_and_numeric_specifications() {
    let modifiers = ChmodModifiers::parse("Fgo-w,D755,ugo=rwX").expect("parse succeeds");
    assert!(!modifiers.is_empty());
}

#[test]
fn parse_rejects_invalid_token() {
    let error = ChmodModifiers::parse("a+q").expect_err("invalid token");
    assert!(error.to_string().contains("unsupported chmod token"));
}

#[cfg(unix)]
#[test]
fn apply_numeric_and_symbolic_modifiers() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().expect("tempdir");
    let file_path = temp.path().join("file.txt");
    let dir_path = temp.path().join("dir");
    std::fs::write(&file_path, b"payload").expect("write file");
    std::fs::create_dir(&dir_path).expect("create dir");
    std::fs::set_permissions(&file_path, PermissionsExt::from_mode(0o666)).expect("set perms");

    let file_type = std::fs::metadata(&file_path)
        .expect("file metadata")
        .file_type();
    let dir_type = std::fs::metadata(&dir_path)
        .expect("dir metadata")
        .file_type();

    let modifiers = ChmodModifiers::parse("Fgo-w,D755").expect("parse");
    let file_mode = modifiers.apply(0o666, file_type);
    assert_eq!(file_mode & 0o777, 0o644);
    let dir_mode = modifiers.apply(0o600, dir_type);
    assert_eq!(dir_mode & 0o777, 0o755);
}

#[cfg(unix)]
#[test]
fn conditional_execute_bit_behaviour_matches_rsync() {
    let temp = tempfile::tempdir().expect("tempdir");
    let file_path = temp.path().join("script.sh");
    let dir_path = temp.path().join("bin");
    std::fs::write(&file_path, b"#!/bin/sh").expect("write file");
    std::fs::create_dir(&dir_path).expect("create dir");

    let file_type = std::fs::metadata(&file_path)
        .expect("file metadata")
        .file_type();
    let dir_type = std::fs::metadata(&dir_path)
        .expect("dir metadata")
        .file_type();

    let modifiers = ChmodModifiers::parse("a+X").expect("parse");
    let file_mode = modifiers.apply(0o644, file_type);
    assert_eq!(file_mode & 0o777, 0o644);
    let dir_mode = modifiers.apply(0o600, dir_type);
    assert_eq!(dir_mode & 0o777, 0o711);
}

/// upstream: chmod.c:parse_chmod() STATE_2ND_HALF accepts a single who-letter
/// on the RHS as a permission-copy source (`copybits`). `g=u` copies the user
/// bits onto the group; `o=g` copies the (updated) group bits onto other.
#[cfg(unix)]
#[test]
fn user_group_copy_clauses_apply_source_permissions() {
    let temp = tempfile::tempdir().expect("tempdir");
    let file_path = temp.path().join("copy.txt");
    std::fs::write(&file_path, b"payload").expect("write file");
    let file_type = std::fs::metadata(&file_path)
        .expect("file metadata")
        .file_type();

    // Mirrors the upstream chmod-option.test permcopy checks.
    let modifiers = ChmodModifiers::parse("g=u").expect("parse");
    // 0o741 (rwxr----x): group := user (rwx) -> 0o771 (rwxrwx--x).
    assert_eq!(modifiers.apply(0o741, file_type) & 0o777, 0o771);

    // g=o then o= : group := other, then clear other. 0o647 -> 0o670.
    let modifiers = ChmodModifiers::parse("g=o,o=").expect("parse");
    assert_eq!(modifiers.apply(0o647, file_type) & 0o777, 0o670);

    // g-o : remove from group the bits set in other. 0o775 -> 0o725.
    let modifiers = ChmodModifiers::parse("g-o").expect("parse");
    assert_eq!(modifiers.apply(0o775, file_type) & 0o777, 0o725);
}

/// Verifies that `D+w` (no explicit who) applies umask masking.
///
/// upstream: chmod.c - when no who-specifier is given, `bits = (where *
/// what) & ~orig_umask`. With a typical umask of 022, `+w` only grants
/// owner-write (0200), NOT group-write or other-write.
#[cfg(unix)]
#[test]
#[allow(unsafe_code)]
fn implied_who_applies_umask_masking() {
    let temp = tempfile::tempdir().expect("tempdir");
    let dir_path = temp.path().join("testdir");
    std::fs::create_dir(&dir_path).expect("create dir");
    let dir_type = std::fs::metadata(&dir_path)
        .expect("dir metadata")
        .file_type();

    // Parse the upstream testsuite spec: ug-s,a+rX,D+w
    let modifiers = ChmodModifiers::parse("ug-s,a+rX,D+w").expect("parse");
    // Starting from 0775 (rwxrwxr-x) which is a common directory default
    let mode = modifiers.apply(0o2775, dir_type);
    // After ug-s: clears setuid+setgid -> 0o775
    // After a+rX: adds read+exec for all (dirs always get exec) -> 0o775
    // After D+w: adds write, but masked by ~umask
    // With umask 022: D+w adds 0o200 (user write only) -> 0o775
    // With umask 000: D+w would add 0o222 (all write) -> 0o777
    // The test just checks that other-write is NOT set when umask blocks it
    let umask = unsafe { libc::umask(0) };
    unsafe { libc::umask(umask) };
    if umask & 0o002 != 0 {
        // umask blocks other-write, so D+w should not grant it
        assert_eq!(
            mode & 0o002,
            0,
            "D+w should not grant other-write when umask blocks it"
        );
    }
}

use super::ChmodModifiers;

#[test]
fn parse_symbolic_and_numeric_specifications() {
    let modifiers = ChmodModifiers::parse("Fgo-w,D755,ugo=rwX").expect("parse succeeds");
    assert!(!modifiers.is_empty());
}

#[test]
fn parse_rejects_invalid_token() {
    let error = ChmodModifiers::parse("a+q").expect_err("invalid token");
    assert!(error.to_string().contains("invalid --chmod specification"));
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
/// (`u`/`g`/`o`) on the right-hand side as a copy-from-category source, and an
/// empty right-hand side clears that class. This is the reported `--chmod`
/// grammar that oc previously rejected.
#[test]
fn who_letter_copy_forms_are_accepted() {
    assert!(ChmodModifiers::parse("g=u").is_ok());
    assert!(ChmodModifiers::parse("g=o,o=").is_ok());
    assert!(ChmodModifiers::parse("g-o").is_ok());
    assert!(ChmodModifiers::parse("u+g").is_ok());
    // A copy letter mixed with a literal permission letter is still an error.
    assert!(ChmodModifiers::parse("g=ur").is_err());
}

/// upstream: chmod.c copy-from-category apply. Resulting mode bits are verified
/// byte-for-byte against `rsync 3.4.3-149` on Linux (umask 022):
/// `g=u` 700->770, `g=o` 707->777, `u=g` 755->555, `g-o` 777->707, `o=` 755->750.
#[cfg(unix)]
#[test]
fn copy_from_category_apply_matches_rsync() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().expect("tempdir");
    let file_path = temp.path().join("f");
    std::fs::write(&file_path, b"payload").expect("write file");
    std::fs::set_permissions(&file_path, PermissionsExt::from_mode(0o644)).expect("set perms");
    let file_type = std::fs::metadata(&file_path).expect("metadata").file_type();

    let apply = |spec: &str, mode: u32| {
        ChmodModifiers::parse(spec)
            .expect("parse")
            .apply(mode, file_type)
            & 0o7777
    };

    assert_eq!(apply("g=u", 0o700), 0o770);
    assert_eq!(apply("g=u", 0o4755), 0o4775);
    assert_eq!(apply("g=o", 0o707), 0o777);
    assert_eq!(apply("u=g", 0o755), 0o555);
    assert_eq!(apply("g-o", 0o777), 0o707);
    assert_eq!(apply("o=", 0o755), 0o750);
    assert_eq!(apply("a=u", 0o700), 0o777);
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

use super::ChmodModifiers;
use super::{directory_transfer_mode, transfer_root_self_locks};

// The following encode upstream's generator dir-permission-during-transfer +
// write-gated restore (generator.c:1512-1520 / 2107-2145). They pin the exact
// final mode a directory carries after the dance, verified against upstream
// rsync 3.4.4: `ug=rw` on a dir yields 0o765 (owner-write kept the transient
// owner-rwx), `u=rx` yields 0o555 (owner lacked write, strict mode restored),
// and an owner-non-executable transfer root self-locks.

#[test]
fn directory_transfer_mode_keeps_owner_rwx_when_owner_writable() {
    // ug=rw -> 0o665: owner writable but not executable; upstream leaves 0o765.
    assert_eq!(directory_transfer_mode(0o665, false), 0o765);
    // Setgid bit survives: 2665 -> 2765.
    assert_eq!(directory_transfer_mode(0o2665, false), 0o2765);
}

#[test]
fn directory_transfer_mode_restores_strict_when_owner_not_writable() {
    // u=rx -> 0o555: owner not writable; touch_up_dirs restores 0o555.
    assert_eq!(directory_transfer_mode(0o555, false), 0o555);
    // u=r -> 0o455: neither write nor execute; strict mode kept.
    assert_eq!(directory_transfer_mode(0o455, false), 0o455);
}

#[test]
fn directory_transfer_mode_noop_when_owner_already_rwx() {
    assert_eq!(directory_transfer_mode(0o775, false), 0o775);
    assert_eq!(directory_transfer_mode(0o755, false), 0o755);
}

#[test]
fn directory_transfer_mode_root_skips_dance() {
    // am_root: no fixup, no restore - the strict tweaked mode passes through.
    assert_eq!(directory_transfer_mode(0o665, true), 0o665);
    assert_eq!(directory_transfer_mode(0o555, true), 0o555);
}

#[test]
fn transfer_root_self_locks_only_without_owner_execute() {
    assert!(transfer_root_self_locks(0o665, false));
    assert!(transfer_root_self_locks(0o424, false));
    assert!(transfer_root_self_locks(0o455, false));
    // Owner keeps execute -> the "." fixup resolves, no self-lock.
    assert!(!transfer_root_self_locks(0o555, false));
    assert!(!transfer_root_self_locks(0o775, false));
    // Root traverses regardless of mode.
    assert!(!transfer_root_self_locks(0o665, true));
}

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

/// upstream: chmod.c:159-185 STATE_2ND_HALF has no `u`/`g`/`o` case, so a
/// category letter in the permission half falls to `default:` -> STATE_ERROR
/// and parse_chmod returns NULL. rsync's `--chmod` grammar has no chmod(1)-style
/// copy-from-category form; upstream 3.4.4 prints
/// `Invalid argument passed to --chmod (g=u)` and exits RERR_SYNTAX. An empty
/// permission half (e.g. `o=`) is a distinct, legitimate clause and still
/// parses (the operator was seen, so it is not an empty clause).
#[test]
fn who_letter_copy_forms_are_rejected() {
    assert!(ChmodModifiers::parse("g=u").is_err());
    assert!(ChmodModifiers::parse("o=g").is_err());
    assert!(ChmodModifiers::parse("u+g").is_err());
    assert!(ChmodModifiers::parse("g-o").is_err());
    assert!(ChmodModifiers::parse("g=ur").is_err());
    // An empty permission half clears the class and remains valid.
    assert!(ChmodModifiers::parse("g=o,o=").is_err());
    assert!(ChmodModifiers::parse("o=").is_ok());
    assert!(ChmodModifiers::parse("Dg=").is_ok());
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

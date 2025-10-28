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

#[cfg(unix)]
#[test]
fn user_group_copy_clauses_are_respected() {
    let temp = tempfile::tempdir().expect("tempdir");
    let file_path = temp.path().join("data.bin");
    std::fs::write(&file_path, b"payload").expect("write file");
    let file_type = std::fs::metadata(&file_path)
        .expect("file metadata")
        .file_type();

    let modifiers = ChmodModifiers::parse("g=u,o=g").expect("parse");
    let mode = modifiers.apply(0o640, file_type);
    assert_eq!(mode & 0o777, 0o666);
}

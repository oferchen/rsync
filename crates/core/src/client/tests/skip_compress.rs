use super::prelude::*;


#[test]
fn skip_compress_from_env_parses_list() {
    let _guard = EnvGuard::set("RSYNC_SKIP_COMPRESS", "gz,zip");
    let list = skip_compress_from_env("RSYNC_SKIP_COMPRESS")
        .expect("parse env list")
        .expect("list present");

    assert!(list.matches_path(Path::new("file.gz")));
    assert!(list.matches_path(Path::new("archive.zip")));
    assert!(!list.matches_path(Path::new("note.txt")));
}


#[test]
fn skip_compress_from_env_absent_returns_none() {
    let _guard = EnvGuard::remove("RSYNC_SKIP_COMPRESS");
    assert!(
        skip_compress_from_env("RSYNC_SKIP_COMPRESS")
            .expect("absent env")
            .is_none()
    );
}


#[test]
fn skip_compress_from_env_reports_invalid_specification() {
    let _guard = EnvGuard::set("RSYNC_SKIP_COMPRESS", "[");
    let error = skip_compress_from_env("RSYNC_SKIP_COMPRESS")
        .expect_err("invalid specification should error");
    let rendered = error.to_string();
    assert!(rendered.contains("RSYNC_SKIP_COMPRESS"));
    assert!(rendered.contains("invalid"));
}


#[cfg(unix)]
#[test]
fn skip_compress_from_env_rejects_non_utf8_values() {
    use std::os::unix::ffi::OsStrExt;

    let bytes = OsStr::from_bytes(&[0xFF]);
    let _guard = EnvGuard::set_os("RSYNC_SKIP_COMPRESS", bytes);
    let error =
        skip_compress_from_env("RSYNC_SKIP_COMPRESS").expect_err("non UTF-8 value should error");
    assert!(
        error
            .to_string()
            .contains("RSYNC_SKIP_COMPRESS accepts only UTF-8")
    );
}


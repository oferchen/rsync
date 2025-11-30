use super::common::*;
use super::*;

#[test]
fn skip_compress_env_variable_enables_list() {
    use tempfile::tempdir;

    let _lock = ENV_LOCK.lock().expect("env mutex poisoned");
    let _guard = EnvGuard::set("RSYNC_SKIP_COMPRESS", OsStr::new("gz"));

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("archive.gz");
    let destination = tmp.path().join("dest.gz");
    std::fs::write(&source, b"payload").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-z"),
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    assert!(stdout.is_empty());
    assert_eq!(std::fs::read(destination).expect("read dest"), b"payload");
}

#[test]
fn skip_compress_invalid_env_reports_error() {
    use tempfile::tempdir;

    let _lock = ENV_LOCK.lock().expect("env mutex poisoned");
    let _guard = EnvGuard::set("RSYNC_SKIP_COMPRESS", OsStr::new("["));

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("file.txt");
    let destination = tmp.path().join("dest.txt");
    std::fs::write(&source, b"payload").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 1);
    assert!(stdout.is_empty());
    let rendered = String::from_utf8(stderr).expect("diagnostic is UTF-8");
    assert!(rendered.contains("RSYNC_SKIP_COMPRESS"));
    assert!(rendered.contains("invalid"));
}

#[test]
fn compress_level_invalid_value_reports_error() {
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--compress-level=fast"),
    ]);

    assert_eq!(code, 1);
    assert!(stdout.is_empty());
    let rendered = String::from_utf8(stderr).expect("diagnostic is valid UTF-8");
    assert!(rendered.contains("--compress-level=fast is invalid"));
}

#[test]
fn compress_level_out_of_range_reports_error() {
    let (code, stdout, stderr) =
        run_with_args([OsString::from(RSYNC), OsString::from("--compress-level=12")]);

    assert_eq!(code, 1);
    assert!(stdout.is_empty());
    let rendered = String::from_utf8(stderr).expect("diagnostic is valid UTF-8");
    assert!(rendered.contains("--compress-level=12 must be between 0 and 9"));
}

#[test]
fn skip_compress_invalid_reports_error() {
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--skip-compress=mp[]"),
    ]);

    assert_eq!(code, 1);
    assert!(stdout.is_empty());
    let rendered = String::from_utf8(stderr).expect("diagnostic is valid UTF-8");
    assert!(rendered.contains("invalid --skip-compress specification"));
    assert!(rendered.contains("empty character class"));
}

#[test]
fn force_no_compress_invalid_env_reports_error() {
    use tempfile::tempdir;

    let _lock = ENV_LOCK.lock().expect("env mutex poisoned");
    let _guard = EnvGuard::set("OC_RSYNC_FORCE_NO_COMPRESS", OsStr::new("maybe"));

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("file.txt");
    let destination = tmp.path().join("dest.txt");
    std::fs::write(&source, b"payload").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(OC_RSYNC),
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 1);
    assert!(stdout.is_empty());

    let rendered = String::from_utf8(stderr).expect("diagnostic is UTF-8");
    assert!(rendered.contains("OC_RSYNC_FORCE_NO_COMPRESS"));
    assert!(rendered.contains("invalid"));
}

#[test]
fn compress_flag_is_accepted_for_local_copies() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("compress.txt");
    let destination = tmp.path().join("compress.out");
    std::fs::write(&source, b"compressed").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-z"),
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    assert_eq!(
        std::fs::read(destination).expect("read destination"),
        b"compressed"
    );
}

#[test]
fn compress_level_flag_is_accepted_for_local_copies() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("compress.txt");
    let destination = tmp.path().join("compress.out");
    std::fs::write(&source, b"payload").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--compress-level=6"),
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    assert_eq!(
        std::fs::read(destination).expect("read destination"),
        b"payload"
    );
}

#[test]
fn compress_level_zero_disables_local_compression() {
    use tempfile::tempdir;

    let _lock = ENV_LOCK.lock().expect("env mutex poisoned");
    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("compress.txt");
    let destination = tmp.path().join("compress.out");
    std::fs::write(&source, b"payload").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--compress-level=0"),
        OsString::from("-z"),
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    assert_eq!(
        std::fs::read(destination).expect("read destination"),
        b"payload"
    );
}

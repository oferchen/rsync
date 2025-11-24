use super::common::*;
use super::*;
use std::os::unix::ffi::OsStringExt;

#[test]
fn server_mode_rejects_missing_flag_string() {
    let _env_lock = ENV_LOCK.lock().expect("env lock");

    let (code, _stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--server"),
        OsString::from("--sender"),
    ]);

    assert_eq!(code, 1);
    let stderr_text = String::from_utf8(stderr).expect("utf8 stderr");
    assert!(stderr_text.contains("missing rsync server flag string"));
    assert_contains_server_trailer(&stderr_text);
}

#[test]
fn server_mode_reports_unimplemented_roles() {
    let _env_lock = ENV_LOCK.lock().expect("env lock");

    let (code, _stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--server"),
        OsString::from("-logDtpre.iLsfxC"),
        OsString::from("."),
        OsString::from("."),
    ]);

    assert_eq!(code, 1);
    let stderr_text = String::from_utf8(stderr).expect("utf8 stderr");
    assert!(stderr_text.contains("native receiver role is not yet implemented"));
    assert_contains_server_trailer(&stderr_text);
}

#[test]
fn server_mode_requires_utf8_flag_string() {
    let _env_lock = ENV_LOCK.lock().expect("env lock");

    let invalid = OsString::from_vec(vec![0xff]);
    let (code, _stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--server"),
        invalid,
        OsString::from("."),
    ]);

    assert_eq!(code, 1);
    let stderr_text = String::from_utf8(stderr).expect("utf8 stderr");
    assert!(stderr_text.contains("flag string must be valid UTF-8"));
    assert_contains_server_trailer(&stderr_text);
}

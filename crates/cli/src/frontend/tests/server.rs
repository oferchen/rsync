use super::common::*;
use super::*;

#[test]
fn server_mode_reports_native_handler_unavailable() {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let exit_code = run(
        [
            OsString::from(RSYNC),
            OsString::from("--server"),
            OsString::from("--sender"),
            OsString::from("."),
            OsString::from("dest"),
        ],
        &mut stdout,
        &mut stderr,
    );

    assert_eq!(exit_code, 1);
    assert!(stdout.is_empty());
    let stderr_text = String::from_utf8(stderr).expect("stderr utf8");
    assert!(
        stderr_text.contains("native --server handling is not yet available"),
        "unexpected stderr: {stderr_text:?}"
    );
    assert_contains_server_trailer(&stderr_text);
}

use super::common::*;
use super::*;

#[test]
fn server_mode_renders_help_and_usage_error() {
    let _env_lock = ENV_LOCK.lock().expect("env lock");

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

    let help = render_missing_operands_stdout(ProgramName::Rsync);
    assert_eq!(stdout, help.as_bytes());

    let stderr_text = String::from_utf8(stderr).expect("stderr utf8");
    assert!(stderr_text.contains("syntax or usage error"));
    assert_contains_server_trailer(&stderr_text);
}

use super::common::*;
use super::*;

#[test]
fn help_flag_renders_static_help_snapshot() {
    let (code, stdout, stderr) = run_with_args([OsStr::new(RSYNC), OsStr::new("--help")]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let expected = render_help(ProgramName::Rsync);
    assert_eq!(stdout, expected.into_bytes());
}

#[test]
fn oc_help_flag_uses_wrapped_program_name() {
    let (code, stdout, stderr) = run_with_args([OsStr::new(OC_RSYNC), OsStr::new("--help")]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let expected = render_help(ProgramName::OcRsync);
    assert_eq!(stdout, expected.into_bytes());
}

#[test]
fn oc_help_mentions_oc_rsyncd_delegation() {
    let (code, stdout, stderr) = run_with_args([OsStr::new(OC_RSYNC), OsStr::new("--help")]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let rendered = String::from_utf8(stdout).expect("valid UTF-8");
    let upstream = format!("delegates to {}", branding::daemon_program_name());
    let branded = format!("delegates to {}", branding::oc_daemon_program_name());
    assert!(rendered.contains(&branded));
    assert!(!rendered.contains(&upstream));
}

#[test]
fn oc_help_mentions_branded_daemon_phrase() {
    let (code, stdout, stderr) = run_with_args([OsStr::new(OC_RSYNC), OsStr::new("--help")]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let rendered = String::from_utf8(stdout).expect("valid UTF-8");
    let upstream = format!("Run as an {} daemon", branding::client_program_name());
    let branded = format!("Run as an {} daemon", branding::oc_client_program_name());
    assert!(rendered.contains(&branded));
    assert!(!rendered.contains(&upstream));
}

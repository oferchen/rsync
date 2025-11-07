use super::common::*;
use super::*;
use crate::frontend::defaults::SUPPORTED_OPTIONS_LIST;
use std::collections::BTreeSet;

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

#[test]
fn supported_options_list_mentions_all_help_flags() {
    let help = render_help(ProgramName::OcRsync);
    let options = collect_options(&help);

    for option in &options {
        assert!(
            SUPPORTED_OPTIONS_LIST.contains(option),
            "supported options list missing {option}"
        );
    }
}

fn collect_options(text: &str) -> BTreeSet<String> {
    let mut tokens = BTreeSet::new();
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '-' {
            match chars.peek() {
                Some('-') => {
                    chars.next();
                    let mut token = String::from("--");
                    while let Some(&next) = chars.peek() {
                        if next.is_ascii_alphanumeric() || next == '-' {
                            token.push(next);
                            chars.next();
                        } else {
                            break;
                        }
                    }
                    if token.len() > 2 {
                        tokens.insert(token);
                    }
                }
                Some(next) if next.is_ascii_alphabetic() => {
                    let mut token = String::from("-");
                    token.push(*next);
                    chars.next();
                    tokens.insert(token);
                }
                _ => {}
            }
        }
    }
    tokens
}

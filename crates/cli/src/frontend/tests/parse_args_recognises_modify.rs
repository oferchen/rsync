use super::common::*;
use super::*;

#[test]
fn parse_args_recognises_modify_window() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--modify-window=5"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");
    assert_eq!(parsed.modify_window, Some(OsString::from("5")));
}

#[test]
fn parse_args_recognises_modify_window_zero() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--modify-window=0"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");
    assert_eq!(parsed.modify_window, Some(OsString::from("0")));
}

#[test]
fn parse_args_accepts_negative_modify_window() {
    // WHY: `--modify-window=-1` is upstream's request for nanosecond-exact
    // comparison (options.c parses a signed int); it must parse successfully and
    // retain the negative value verbatim rather than be rejected.
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--modify-window=-1"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("negative modify-window should parse");
    assert_eq!(parsed.modify_window, Some(OsString::from("-1")));
}

#[test]
fn parse_args_accepts_short_at_alias_modify_window() {
    // WHY: upstream options.c:670 defines `-@` as the short alias for
    // `--modify-window`. `-@-1`, `-@2`, and `-@ 2` must all parse to the same
    // value the long form yields.
    for (arg, expected) in [("-@-1", "-1"), ("-@2", "2")] {
        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from(arg),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .unwrap_or_else(|e| panic!("{arg} should parse: {e}"));
        assert_eq!(parsed.modify_window, Some(OsString::from(expected)));
    }
}

#[test]
fn parse_args_rejects_non_numeric_modify_window() {
    let error = match parse_args([
        OsString::from(RSYNC),
        OsString::from("--modify-window=fat"),
        OsString::from("source"),
        OsString::from("dest"),
    ]) {
        Ok(_) => panic!("parse should fail"),
        Err(error) => error,
    };
    assert_eq!(error.kind(), clap::error::ErrorKind::ValueValidation);
    assert!(
        error
            .to_string()
            .contains("invalid --modify-window value 'fat'")
    );
}

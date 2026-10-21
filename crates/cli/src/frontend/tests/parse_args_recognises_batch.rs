use super::common::*;
use super::*;

#[test]
fn parse_args_recognises_write_batch_prefix() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--write-batch=updates"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.write_batch, Some(OsString::from("updates")));
    assert!(parsed.only_write_batch.is_none());
    assert!(parsed.read_batch.is_none());
}

#[test]
fn parse_args_recognises_only_write_batch_prefix() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--only-write-batch=batch"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.only_write_batch, Some(OsString::from("batch")));
    assert!(parsed.write_batch.is_none());
    assert!(parsed.read_batch.is_none());
}

/// upstream: options.c:2280-2283 - a dry run records nothing, so `-n` must
/// drop both write forms; a surviving name would create the batch and `.sh`
/// that rsync 3.5.1 never writes under `-n`.
#[test]
fn parse_args_dry_run_discards_both_write_batch_forms() {
    for option in ["--write-batch=updates", "--only-write-batch=updates"] {
        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from("-n"),
            OsString::from(option),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert!(parsed.write_batch.is_none(), "{option}");
        assert!(parsed.only_write_batch.is_none(), "{option}");
    }
}

/// The dry-run rule zeroes only `write_batch`; `--read-batch -n` still
/// replays (as a dry run), so the read name must survive.
#[test]
fn parse_args_dry_run_keeps_read_batch() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("-n"),
        OsString::from("--read-batch=replay"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.read_batch, Some(OsString::from("replay")));
}

/// upstream: options.c:1780-1788 - both write forms set the single
/// `write_batch` mode, so combining them is legal and the later one wins.
#[test]
fn parse_args_later_write_batch_form_wins() {
    let parse = |first: &str, second: &str| {
        parse_args([
            OsString::from(RSYNC),
            OsString::from(first),
            OsString::from(second),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("both write forms together are legal")
    };
    let parsed = parse("--write-batch=a", "--only-write-batch=b");
    assert_eq!(parsed.only_write_batch, Some(OsString::from("b")));
    assert!(parsed.write_batch.is_none());
    let parsed = parse("--only-write-batch=a", "--write-batch=b");
    assert_eq!(parsed.write_batch, Some(OsString::from("b")));
    assert!(parsed.only_write_batch.is_none());
}

/// upstream: options.c:2265-2269 - a write form with `--read-batch` is
/// refused with this exact text, whichever order or write form is used.
#[test]
fn parse_args_refuses_write_with_read_batch() {
    for (first, second) in [
        ("--write-batch=a", "--read-batch=b"),
        ("--read-batch=b", "--only-write-batch=a"),
    ] {
        let err = parse_args([
            OsString::from(RSYNC),
            OsString::from(first),
            OsString::from(second),
            OsString::from("dest"),
        ])
        .expect_err("write and read batch together");
        assert!(
            err.to_string()
                .contains("--write-batch and --read-batch can not be used together"),
            "{err}"
        );
    }
}

#[test]
fn parse_args_recognises_read_batch_prefix() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--read-batch=replay"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.read_batch, Some(OsString::from("replay")));
    assert!(parsed.write_batch.is_none());
    assert!(parsed.only_write_batch.is_none());
}

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

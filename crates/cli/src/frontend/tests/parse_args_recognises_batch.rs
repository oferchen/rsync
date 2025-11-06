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

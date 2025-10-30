use super::common::*;
use super::*;

#[test]
fn parse_args_recognises_partial_dir_and_enables_partial() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--partial-dir=.rsync-partial"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(parsed.partial);
    assert_eq!(
        parsed.partial_dir.as_deref(),
        Some(Path::new(".rsync-partial"))
    );
}

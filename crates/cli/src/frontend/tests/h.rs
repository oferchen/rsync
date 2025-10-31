use super::common::*;
use super::*;

#[test]
fn short_h_flag_enables_human_readable_mode() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("-h"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.human_readable, Some(HumanReadableMode::Enabled));
}

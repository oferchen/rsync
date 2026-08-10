use std::ffi::OsString;

use super::common::*;
use super::*;

#[test]
fn parse_args_recognises_eight_bit_output_flag() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--8-bit-output"),
        OsString::from("src"),
        OsString::from("dst"),
    ])
    .expect("parse succeeds");

    assert!(parsed.eight_bit_output);
}

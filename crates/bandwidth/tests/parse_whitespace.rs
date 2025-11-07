use oc_rsync_bandwidth::{BandwidthParseError, parse_bandwidth_argument, parse_bandwidth_limit};

fn assert_invalid_argument(text: &str) {
    let err = parse_bandwidth_argument(text).expect_err("argument should be invalid");
    assert_eq!(err, BandwidthParseError::Invalid);
}

fn assert_invalid_limit(text: &str) {
    let err = parse_bandwidth_limit(text).expect_err("limit should be invalid");
    assert_eq!(err, BandwidthParseError::Invalid);
}

#[test]
fn rejects_leading_whitespace_argument() {
    assert_invalid_argument(" 1024");
}

#[test]
fn rejects_trailing_whitespace_argument() {
    assert_invalid_argument("1024 ");
}

#[test]
fn rejects_internal_whitespace_argument() {
    assert_invalid_argument("10 24");
}

#[test]
fn rejects_limit_with_leading_whitespace() {
    assert_invalid_limit(" 2048");
}

#[test]
fn rejects_limit_with_trailing_whitespace() {
    assert_invalid_limit("2048 ");
}

#[test]
fn rejects_limit_with_whitespace_around_separator() {
    assert_invalid_limit("2048 :1024");
    assert_invalid_limit("2048: 1024");
}

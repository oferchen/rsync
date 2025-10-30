use super::common::*;
use super::*;

#[test]
fn chown_requires_non_empty_components() {
    let error = parse_chown_argument(OsStr::new("")).expect_err("empty --chown spec should fail");
    let rendered = error.to_string();
    assert!(
        rendered.contains("--chown requires a non-empty USER and/or GROUP"),
        "diagnostic missing non-empty message: {rendered}"
    );

    let colon_error =
        parse_chown_argument(OsStr::new(":")).expect_err("missing user and group should fail");
    let colon_rendered = colon_error.to_string();
    assert!(
        colon_rendered.contains("--chown requires a user and/or group"),
        "diagnostic missing user/group message: {colon_rendered}"
    );
}

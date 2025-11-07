#[test]
fn runtime_options_reject_invalid_bwlimit() {
    let error = RuntimeOptions::parse(&[OsString::from("--bwlimit"), OsString::from("foo")])
        .expect_err("invalid bwlimit should fail");

    assert!(
        error
            .message()
            .to_string()
            .contains("--bwlimit=foo is invalid")
    );
}


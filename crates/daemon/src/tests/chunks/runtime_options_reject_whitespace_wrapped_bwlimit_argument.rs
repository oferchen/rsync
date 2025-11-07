#[test]
fn runtime_options_reject_whitespace_wrapped_bwlimit_argument() {
    let error = RuntimeOptions::parse(&[OsString::from("--bwlimit"), OsString::from(" 8M \n")])
        .expect_err("whitespace-wrapped bwlimit should fail");

    assert!(
        error
            .message()
            .to_string()
            .contains("--bwlimit= 8M \n is invalid")
    );
}


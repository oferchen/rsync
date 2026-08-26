#[test]
fn runtime_options_reject_duplicate_bwlimit() {
    let error = RuntimeOptions::parse(&[
        OsString::from("--bwlimit"),
        OsString::from("8192"),
        OsString::from("--bwlimit"),
        OsString::from("16384"),
    ])
    .expect_err("duplicate bwlimit should fail");

    assert!(
        error
            .message()
            .to_string()
            .contains("duplicate daemon argument '--bwlimit'")
    );
}

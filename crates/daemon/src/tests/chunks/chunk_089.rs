#[test]
fn parse_config_modules_detects_recursive_include() {
    let dir = tempdir().expect("tempdir");
    let first = dir.path().join("first.conf");
    let second = dir.path().join("second.conf");

    writeln!(
        File::create(&first).expect("create first"),
        "include = second.conf\n"
    )
    .expect("write first");
    writeln!(
        File::create(&second).expect("create second"),
        "include = first.conf\n"
    )
    .expect("write second");

    let error = parse_config_modules(&first).expect_err("recursive include should fail");
    assert!(error.message().to_string().contains("recursive include"));
}


#[test]
fn runtime_options_invalid_boolean_keeps_default() {
    // upstream: loadparm.c:418-423 - do_parameter() ignores set_boolean()'s
    // failure, so a badly formed boolean warns and retains the directive's
    // default rather than aborting the load. `read only` defaults to true.
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(file, "[docs]\npath = /srv/docs\nread only = maybe\n").expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("badly formed boolean must not abort the load");

    assert!(options.modules()[0].read_only());
}

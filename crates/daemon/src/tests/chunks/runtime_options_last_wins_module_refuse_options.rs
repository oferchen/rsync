/// A `refuse options` directive repeated inside one module section replaces the
/// earlier list instead of failing the load.
///
/// upstream: loadparm.c:379-470 do_parameter() assigns unconditionally, so
/// rsync 3.4.4 refuses only the options the last directive named.
#[test]
fn runtime_options_last_wins_module_refuse_options() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(
        file,
        "[docs]\npath = /srv/docs\nrefuse options = delete\nrefuse options = compress\n"
    )
    .expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("repeated refuse options is accepted");

    assert_eq!(options.modules()[0].refused_options(), ["compress"]);
}

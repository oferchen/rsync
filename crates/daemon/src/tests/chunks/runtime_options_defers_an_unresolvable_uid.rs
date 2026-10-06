/// A `uid` name the host cannot resolve loads and is refused per module.
///
/// upstream: clientserver.c:833-838 resolves `uid` when a client selects the
/// module and replies `@ERROR: invalid uid <name>`.
#[test]
fn runtime_options_defers_an_unresolvable_uid() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(file, "[docs]\npath = /srv/docs\nuid = alpha\n").expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("an unresolvable uid must load");

    assert_eq!(
        options.modules()[0].unresolved_id,
        Some(UnresolvedId::Uid("alpha".to_owned()))
    );
}

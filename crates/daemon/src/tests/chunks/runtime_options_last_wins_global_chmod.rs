/// An `incoming chmod` directive repeated in the global section keeps its last
/// value and every module inherits that value.
///
/// upstream: loadparm.c:452-454 `case P_STRING: string_set(parm_ptr,
/// parmvalue)` overwrites the slot, so only the final value survives.
#[test]
fn runtime_options_last_wins_global_chmod() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(
        file,
        "incoming chmod = Duog\nincoming chmod = Fu\n[docs]\npath = /srv/docs\n"
    )
    .expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("repeated incoming chmod is accepted");

    assert_eq!(options.modules()[0].incoming_chmod(), Some("Fu"));
}

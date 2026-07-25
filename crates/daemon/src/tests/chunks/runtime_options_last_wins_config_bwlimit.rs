/// A `bwlimit` directive repeated inside one module section keeps its last
/// value instead of failing the load.
///
/// upstream: loadparm.c:379-470 do_parameter() assigns the resolved parameter
/// pointer unconditionally, so rsync 3.4.4 starts and serves the module.
#[test]
fn runtime_options_last_wins_config_bwlimit() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(
        file,
        "[docs]\npath = /srv/docs\nbwlimit = 1M\nbwlimit = 2M\n"
    )
    .expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("repeated bwlimit is accepted");

    let modules = options.modules();
    assert_eq!(modules.len(), 1);
    assert_eq!(modules[0].bandwidth_limit(), NonZeroU64::new(2 * 1024 * 1024));
}

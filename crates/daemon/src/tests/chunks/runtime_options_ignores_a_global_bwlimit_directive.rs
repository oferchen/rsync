/// `bwlimit` is not an rsyncd.conf parameter upstream, so a global
/// `bwlimit` line is reported and ignored: the config loads and the daemon's
/// bandwidth limit stays unset.
///
/// upstream: daemon-parm.txt has no `bwlimit` parameter; loadparm.c
/// map_parameter()/do_parameter() log it as unknown and carry on. The daemon's
/// limit comes only from `--bwlimit` (options.c:876).
#[test]
fn runtime_options_ignores_a_global_bwlimit_directive() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(file, "bwlimit = 100\n[docs]\npath = /srv/docs\n").expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("an unknown global parameter is ignored, not a hard error");

    assert_eq!(options.modules().len(), 1);
    assert!(options.bandwidth_limit().is_none());
}

/// A `bwlimit` directive repeated in the global section keeps its last value.
///
/// upstream: loadparm.c:379-470 do_parameter() - no seen-set, no duplicate
/// check, so the second assignment simply overwrites the first.
#[test]
fn runtime_options_last_wins_global_bwlimit() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(
        file,
        "bwlimit = 1M\nbwlimit = 2M\n[docs]\npath = /srv/docs\n"
    )
    .expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("repeated global bwlimit is accepted");

    assert_eq!(options.bandwidth_limit(), NonZeroU64::new(2 * 1024 * 1024));
}

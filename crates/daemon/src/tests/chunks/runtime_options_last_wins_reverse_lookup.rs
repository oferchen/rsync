/// A `reverse lookup` directive repeated in the global section keeps its last
/// value.
///
/// upstream: loadparm.c:379-470 do_parameter() assigns the BOOL slot
/// unconditionally, so the second `no` wins over the first `yes`.
#[test]
fn runtime_options_last_wins_reverse_lookup() {
    let dir = tempdir().expect("config dir");
    let config_path = dir.path().join("rsyncd.conf");
    fs::write(
        &config_path,
        "reverse lookup = yes\nreverse lookup = no\n[docs]\npath = /srv/docs\n",
    )
    .expect("write config");

    let args = [
        OsString::from("--config"),
        config_path.as_os_str().to_os_string(),
    ];
    let options = RuntimeOptions::parse(&args).expect("repeated reverse lookup is accepted");
    assert!(!options.reverse_lookup());
}

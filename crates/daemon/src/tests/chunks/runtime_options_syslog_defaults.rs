#[test]
fn runtime_options_syslog_facility_defaults_to_daemon() {
    let options = RuntimeOptions::default();
    assert_eq!(options.syslog_facility(), "daemon");
}

#[test]
fn runtime_options_syslog_tag_defaults_to_oc_rsyncd() {
    let options = RuntimeOptions::default();
    assert_eq!(options.syslog_tag(), "oc-rsyncd");
}

#[test]
fn runtime_options_module_definition_parses_inline_bwlimit_burst() {
    let options = RuntimeOptions::parse(&[
        OsString::from("--module"),
        OsString::from("mirror=./data;use-chroot=no;bwlimit=2m:8m"),
    ])
    .expect("parse module with inline bwlimit burst");

    let modules = options.modules();
    assert_eq!(modules.len(), 1);
    let module = &modules[0];
    assert_eq!(
        module.bandwidth_limit().map(NonZeroU64::get),
        Some(2_097_152)
    );
    assert_eq!(
        module.bandwidth_burst().map(NonZeroU64::get),
        Some(8_388_608)
    );
    assert!(module.bandwidth_burst_specified());
}


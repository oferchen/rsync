#[test]
fn runtime_options_inline_module_overrides_chmod() {
    let options = RuntimeOptions::parse(&[
        OsString::from("--module"),
        OsString::from("cli=/srv/cli;incoming chmod=Fx;outgoing chmod=Dx"),
    ])
    .expect("parse inline module");

    let module = &options.modules()[0];
    assert_eq!(module.name, "cli");
    assert_eq!(module.incoming_chmod(), Some("Fx"));
    assert_eq!(module.outgoing_chmod(), Some("Dx"));
}

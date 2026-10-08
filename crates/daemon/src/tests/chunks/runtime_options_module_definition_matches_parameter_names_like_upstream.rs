// upstream: loadparm.c:401 strwiEQ ignores case and whitespace, so every
// spelling below names the `read only` and `incoming chmod` parameters.
#[test]
fn runtime_options_module_definition_matches_parameter_names_like_upstream() {
    for spec in [
        "docs=/srv/docs;readonly=yes;incomingchmod=Fx",
        "docs=/srv/docs;Read Only=yes;Incoming  Chmod=Fx",
    ] {
        let options = RuntimeOptions::parse(&[OsString::from("--module"), OsString::from(spec)])
            .expect("parameter name spelling should be accepted");
        let module = &options.modules()[0];
        assert!(module.read_only(), "read only not applied for {spec}");
        assert_eq!(module.incoming_chmod(), Some("Fx"), "spec {spec}");
    }
}

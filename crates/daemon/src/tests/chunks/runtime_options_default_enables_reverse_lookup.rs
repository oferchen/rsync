#[test]
fn runtime_options_default_enables_reverse_lookup() {
    let options = RuntimeOptions::parse(&[]).expect("parse defaults");
    assert!(options.reverse_lookup());
}


#[test]
fn buffered_prefix_too_small_converts_to_io_error_with_context() {
    let err = BufferedPrefixTooSmall::new(LEGACY_DAEMON_PREFIX_LEN, 4);
    let message = err.to_string();
    let required = err.required();
    let available = err.available();
    let missing = err.missing();

    let io_err: io::Error = err.into();

    assert_eq!(io_err.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(io_err.to_string(), message);

    let source = io_err
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<BufferedPrefixTooSmall>())
        .expect("io::Error must retain BufferedPrefixTooSmall source");
    assert_eq!(source.required(), required);
    assert_eq!(source.available(), available);
    assert_eq!(source.missing(), missing);
}

#[test]
fn buffered_prefix_too_small_reports_missing_bytes() {
    let err = BufferedPrefixTooSmall::new(LEGACY_DAEMON_PREFIX_LEN, LEGACY_DAEMON_PREFIX_LEN - 3);
    assert_eq!(err.missing(), 3);

    let saturated = BufferedPrefixTooSmall::new(4, 8);
    assert_eq!(saturated.missing(), 0);
}

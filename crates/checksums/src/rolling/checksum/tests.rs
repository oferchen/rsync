use super::*;

#[test]
fn empty_reader_buffer_is_rejected() {
    let mut c = RollingChecksum::new();
    let mut rdr = &b""[..];
    let mut buf: [u8; 0] = [];
    let err = c.update_reader_with_buffer(&mut rdr, &mut buf).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[test]
fn x86_cpu_feature_detection_is_cached() {
    x86::load_cpu_features_for_tests();
    assert!(x86::cpu_features_cached_for_tests());
}

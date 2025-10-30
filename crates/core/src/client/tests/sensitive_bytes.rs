use super::prelude::*;


#[test]
fn sensitive_bytes_zeroizes_on_drop() {
    let bytes = SensitiveBytes::new(b"topsecret".to_vec());
    let zeroed = bytes.into_zeroized_vec();
    assert!(zeroed.iter().all(|&byte| byte == 0));
}


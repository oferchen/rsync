use super::prelude::*;


#[test]
fn daemon_auth_context_zeroizes_secret_on_drop() {
    let context = DaemonAuthContext::new("user".to_string(), b"supersecret".to_vec());
    let zeroed = context.into_zeroized_secret();
    assert!(zeroed.iter().all(|&byte| byte == 0));
}


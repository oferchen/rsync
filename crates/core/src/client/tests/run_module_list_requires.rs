use super::prelude::*;


#[test]
fn run_module_list_requires_password_for_authentication() {
    let responses = vec!["@RSYNCD: AUTHREQD challenge\n", "@RSYNCD: EXIT\n"];
    let (addr, handle) = spawn_stub_daemon(responses);

    let request = ModuleListRequest::from_components(
        DaemonAddress::new(addr.ip().to_string(), addr.port()).expect("address"),
        Some(String::from("user")),
        ProtocolVersion::NEWEST,
    );

    let _guard = env_lock().lock().unwrap();
    set_test_daemon_password(None);

    let error = run_module_list(request).expect_err("missing password should fail");
    assert_eq!(error.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
    assert!(error.message().to_string().contains("RSYNC_PASSWORD"));

    handle.join().expect("server thread");
}


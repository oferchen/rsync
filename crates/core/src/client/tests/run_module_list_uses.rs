use super::prelude::*;


#[test]
fn run_module_list_uses_connect_program_command() {
    let _guard = env_lock().lock().expect("env mutex poisoned");

    let command = OsString::from(
        "sh -c 'CONNECT_HOST=%H\n\
         CONNECT_PORT=%P\n\
         printf \"@RSYNCD: 31.0\\n\"\n\
         read greeting\n\
         printf \"@RSYNCD: OK\\n\"\n\
         read request\n\
         printf \"example\\t$CONNECT_HOST:$CONNECT_PORT\\n@RSYNCD: EXIT\\n\"'",
    );

    let _prog_guard = EnvGuard::set_os("RSYNC_CONNECT_PROG", &command);
    let _shell_guard = EnvGuard::remove("RSYNC_SHELL");
    let _proxy_guard = EnvGuard::remove("RSYNC_PROXY");

    let request = ModuleListRequest::from_components(
        DaemonAddress::new("example.com".to_string(), 873).expect("address"),
        None,
        ProtocolVersion::NEWEST,
    );

    let list = run_module_list(request).expect("connect program listing succeeds");
    assert_eq!(list.entries().len(), 1);
    let entry = &list.entries()[0];
    assert_eq!(entry.name(), "example");
    assert_eq!(entry.comment(), Some("example.com:873"));
}


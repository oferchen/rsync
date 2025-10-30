use super::prelude::*;


#[cfg(unix)]
#[test]
fn remote_fallback_invocation_forwards_streams() {
    let _lock = env_lock().lock().expect("env mutex poisoned");
    let temp = tempdir().expect("tempdir created");
    let script = write_fallback_script(temp.path());

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut args = baseline_fallback_args();
    args.files_from_used = true;
    args.file_list_entries = vec![OsString::from("alpha"), OsString::from("beta")];
    args.fallback_binary = Some(script.into_os_string());

    let exit_code = run_remote_transfer_fallback(&mut stdout, &mut stderr, args)
        .expect("fallback invocation succeeds");

    assert_eq!(exit_code, 42);
    assert_eq!(
        String::from_utf8(stdout).expect("stdout utf8"),
        "alpha\nbeta\nfallback stdout\n"
    );
    assert_eq!(
        String::from_utf8(stderr).expect("stderr utf8"),
        "fallback stderr\n"
    );
}


use super::prelude::*;


#[cfg(unix)]
#[test]
fn run_client_or_fallback_uses_fallback_for_remote_operands() {
    let _lock = env_lock().lock().expect("env mutex poisoned");
    let temp = tempdir().expect("tempdir created");
    let script = write_fallback_script(temp.path());

    let config = ClientConfig::builder()
        .transfer_args([OsString::from("remote::module"), OsString::from("/tmp/dst")])
        .build();

    let mut args = baseline_fallback_args();
    args.remainder = vec![OsString::from("remote::module"), OsString::from("/tmp/dst")];
    args.fallback_binary = Some(script.into_os_string());

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let context = RemoteFallbackContext::new(&mut stdout, &mut stderr, args);

    let outcome =
        run_client_or_fallback(config, None, Some(context)).expect("fallback invocation succeeds");

    match outcome {
        ClientOutcome::Fallback(summary) => {
            assert_eq!(summary.exit_code(), 42);
        }
        ClientOutcome::Local(_) => panic!("expected fallback outcome"),
    }

    assert_eq!(
        String::from_utf8(stdout).expect("stdout utf8"),
        "fallback stdout\n"
    );
    assert_eq!(
        String::from_utf8(stderr).expect("stderr utf8"),
        "fallback stderr\n"
    );
}


#[cfg(unix)]
#[test]
fn run_client_or_fallback_handles_delta_mode_locally() {
    let _lock = env_lock().lock().expect("env mutex poisoned");
    let temp = tempdir().expect("tempdir created");
    let script = write_fallback_script(temp.path());

    let source_path = temp.path().join("source.txt");
    let dest_path = temp.path().join("dest.txt");
    fs::write(&source_path, b"delta-test").expect("source created");

    let source = OsString::from(source_path.as_os_str());
    let dest = OsString::from(dest_path.as_os_str());

    let config = ClientConfig::builder()
        .transfer_args([source.clone(), dest.clone()])
        .whole_file(false)
        .build();

    let mut args = baseline_fallback_args();
    args.remainder = vec![source, dest];
    args.whole_file = Some(false);
    args.fallback_binary = Some(script.into_os_string());

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let context = RemoteFallbackContext::new(&mut stdout, &mut stderr, args);

    let outcome =
        run_client_or_fallback(config, None, Some(context)).expect("local delta copy succeeds");

    match outcome {
        ClientOutcome::Local(summary) => {
            assert_eq!(summary.files_copied(), 1);
        }
        ClientOutcome::Fallback(_) => panic!("unexpected fallback execution"),
    }

    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    assert_eq!(fs::read(dest_path).expect("dest contents"), b"delta-test");
}


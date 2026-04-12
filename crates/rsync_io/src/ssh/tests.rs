use super::SshCommand;
#[cfg(unix)]
use super::SshConnection;
use std::ffi::{OsStr, OsString};
#[cfg(unix)]
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

fn args_to_strings(args: &[OsString]) -> Vec<String> {
    args.iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect()
}

#[test]
fn assembles_minimal_command_with_batch_mode() {
    let command = SshCommand::new("example.com");
    let (program, args) = command.command_parts_for_testing();

    assert_eq!(program, OsString::from("ssh"));
    assert_eq!(
        args_to_strings(&args),
        vec![
            "-oBatchMode=yes".to_owned(),
            "-oServerAliveInterval=20".to_owned(),
            "-oServerAliveCountMax=3".to_owned(),
            "example.com".to_owned(),
        ]
    );
}

#[test]
fn assembles_command_with_user_port_and_remote_args() {
    let mut command = SshCommand::new("rsync.example.com");
    command.set_user("backup");
    command.set_port(2222);
    command.push_option("-vvv");
    command.push_remote_arg("rsync");
    command.push_remote_arg("--server");
    command.push_remote_arg(".");

    let (_, args) = command.command_parts_for_testing();
    let rendered = args_to_strings(&args);

    assert_eq!(
        rendered,
        vec![
            "-oBatchMode=yes".to_owned(),
            "-p".to_owned(),
            "2222".to_owned(),
            "-oServerAliveInterval=20".to_owned(),
            "-oServerAliveCountMax=3".to_owned(),
            "-vvv".to_owned(),
            "backup@rsync.example.com".to_owned(),
            "rsync".to_owned(),
            "--server".to_owned(),
            ".".to_owned(),
        ]
    );
}

#[test]
fn disables_batch_mode_when_requested() {
    let mut command = SshCommand::new("example.com");
    command.set_batch_mode(false);

    let (_, args) = command.command_parts_for_testing();
    assert_eq!(
        args_to_strings(&args),
        vec![
            "-oServerAliveInterval=20".to_owned(),
            "-oServerAliveCountMax=3".to_owned(),
            "example.com".to_owned(),
        ]
    );
}

#[test]
fn wraps_ipv6_hosts_in_brackets() {
    let command = SshCommand::new("2001:db8::1");
    let (_, args) = command.command_parts_for_testing();

    assert_eq!(
        args_to_strings(&args),
        vec![
            "-oBatchMode=yes".to_owned(),
            "-oServerAliveInterval=20".to_owned(),
            "-oServerAliveCountMax=3".to_owned(),
            "[2001:db8::1]".to_owned(),
        ]
    );
}

#[test]
fn wraps_ipv6_hosts_with_usernames() {
    let mut command = SshCommand::new("2001:db8::1");
    command.set_user("backup");

    let (_, args) = command.command_parts_for_testing();

    assert_eq!(
        args_to_strings(&args),
        vec![
            "-oBatchMode=yes".to_owned(),
            "-oServerAliveInterval=20".to_owned(),
            "-oServerAliveCountMax=3".to_owned(),
            "backup@[2001:db8::1]".to_owned(),
        ]
    );
}

#[test]
fn preserves_explicit_bracketed_ipv6_literals() {
    let mut command = SshCommand::new("[2001:db8::1]");
    command.set_user("backup");

    let (_, args) = command.command_parts_for_testing();

    assert_eq!(
        args_to_strings(&args),
        vec![
            "-oBatchMode=yes".to_owned(),
            "-oServerAliveInterval=20".to_owned(),
            "-oServerAliveCountMax=3".to_owned(),
            "backup@[2001:db8::1]".to_owned(),
        ]
    );
}

#[test]
fn command_parts_skip_target_when_host_and_user_missing() {
    let mut command = SshCommand::new("");
    command.set_remote_command([OsString::from("rsync")]);

    let (_, args) = command.command_parts_for_testing();

    assert_eq!(
        args_to_strings(&args),
        vec![
            "-oBatchMode=yes".to_owned(),
            "-oServerAliveInterval=20".to_owned(),
            "-oServerAliveCountMax=3".to_owned(),
            "rsync".to_owned(),
        ]
    );
}

#[test]
fn target_override_supersedes_computed_target() {
    let mut command = SshCommand::new("example.com");
    command.set_user("backup");
    command.set_target_override(Some("custom-target"));

    let (_, args) = command.command_parts_for_testing();

    assert_eq!(
        args_to_strings(&args),
        vec![
            "-oBatchMode=yes".to_owned(),
            "-oServerAliveInterval=20".to_owned(),
            "-oServerAliveCountMax=3".to_owned(),
            "custom-target".to_owned(),
        ]
    );
}

#[test]
fn empty_target_override_suppresses_target_argument() {
    let mut command = SshCommand::new("example.com");
    command.set_target_override(Some(OsString::new()));
    command.push_remote_arg("rsync");

    let (_, args) = command.command_parts_for_testing();

    assert_eq!(
        args_to_strings(&args),
        vec![
            "-oBatchMode=yes".to_owned(),
            "-oServerAliveInterval=20".to_owned(),
            "-oServerAliveCountMax=3".to_owned(),
            "rsync".to_owned(),
        ]
    );
}

#[cfg(unix)]
fn spawn_echo_process() -> SshConnection {
    let mut command = SshCommand::new("ignored");
    command.set_program("sh");
    command.set_batch_mode(false);
    command.push_option("-c");
    command.push_option("cat");

    command
        .spawn()
        .expect("failed to spawn local shell for testing")
}

#[cfg(unix)]
#[test]
fn spawned_connection_forwards_io() {
    let mut connection = spawn_echo_process();

    connection.write_all(b"abc").expect("write payload");
    connection.flush().expect("flush payload");

    let mut buffer = [0u8; 3];
    connection.read_exact(&mut buffer).expect("read echo");
    assert_eq!(&buffer, b"abc");

    let status = connection.wait().expect("wait for process");
    assert!(status.success());
}

#[cfg(unix)]
#[test]
fn stderr_output_collected_via_child_handle() {
    let mut command = SshCommand::new("ignored");
    command.set_program("sh");
    command.set_batch_mode(false);
    command.push_option("-c");
    command.push_option("printf 'err-line\\n' >&2");
    command.set_target_override(Some(OsString::new()));

    let connection = command.spawn().expect("spawn shell");
    let (_reader, _writer, child_handle) = connection.split().expect("split");

    let (status, stderr) = child_handle.wait_with_stderr().expect("wait status");
    assert!(status.success());

    let text = String::from_utf8_lossy(&stderr);
    assert!(
        text.contains("err-line"),
        "expected stderr to contain 'err-line', got: {text}"
    );
}

#[cfg(unix)]
#[test]
fn stderr_output_empty_when_no_errors() {
    let connection = spawn_echo_process();
    let (_reader, writer, child_handle) = connection.split().expect("split");
    writer.close().expect("close writer");

    let (status, stderr) = child_handle.wait_with_stderr().expect("wait");
    assert!(status.success());
    assert!(stderr.is_empty(), "expected empty stderr, got: {stderr:?}");
}

#[cfg(unix)]
#[test]
fn stderr_output_bounded_to_cap() {
    let mut command = SshCommand::new("ignored");
    command.set_program("sh");
    command.set_batch_mode(false);
    command.push_option("-c");
    // Generate ~128 KB of stderr output to exceed the 64 KB cap.
    command.push_option(
        "i=0; while [ $i -lt 2048 ]; do printf 'AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\\n' >&2; i=$((i+1)); done",
    );
    command.set_target_override(Some(OsString::new()));

    let connection = command.spawn().expect("spawn shell");
    let (_reader, _writer, child_handle) = connection.split().expect("split");

    let (status, stderr) = child_handle.wait_with_stderr().expect("wait");
    assert!(status.success());

    // Buffer should be bounded to 64 KB.
    let cap = 64 * 1024;
    assert!(
        stderr.len() <= cap,
        "stderr buffer should be bounded to {cap}, got {}",
        stderr.len()
    );
    // Should have collected some output (not empty).
    assert!(!stderr.is_empty(), "expected non-empty stderr output");
}

#[cfg(unix)]
#[test]
fn stderr_output_readable_before_wait() {
    use std::time::{Duration, Instant};

    let mut command = SshCommand::new("ignored");
    command.set_program("sh");
    command.set_batch_mode(false);
    command.push_option("-c");
    command.push_option("printf 'early-msg\\n' >&2");
    command.set_target_override(Some(OsString::new()));

    let connection = command.spawn().expect("spawn shell");
    let (_reader, _writer, child_handle) = connection.split().expect("split");

    // Poll stderr_output() before wait() - the drain thread collects
    // output asynchronously, so we poll until it arrives.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let output = child_handle.stderr_output();
        if !output.is_empty() {
            let text = String::from_utf8_lossy(&output);
            assert!(
                text.contains("early-msg"),
                "expected 'early-msg', got: {text}"
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for stderr output"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    let status = child_handle.wait().expect("wait");
    assert!(status.success());
}

#[test]
fn configure_remote_shell_updates_program_and_options() {
    let mut command = SshCommand::new("example.com");
    command
        .configure_remote_shell(OsStr::new("ssh -p 2222"))
        .expect("configure succeeds");

    let (program, args) = command.command_parts_for_testing();
    assert_eq!(program, OsString::from("ssh"));
    assert!(args.contains(&OsString::from("-p")));
    assert!(args.contains(&OsString::from("2222")));
}

#[test]
fn parse_remote_shell_with_multiple_options() {
    use super::parse_remote_shell;

    let spec = OsStr::new("ssh -p 2222 -o StrictHostKeyChecking=no");
    let args = parse_remote_shell(spec).expect("parsing should succeed");

    assert_eq!(args.len(), 5);
    assert_eq!(args[0].to_string_lossy(), "ssh");
    assert_eq!(args[1].to_string_lossy(), "-p");
    assert_eq!(args[2].to_string_lossy(), "2222");
    assert_eq!(args[3].to_string_lossy(), "-o");
    assert_eq!(args[4].to_string_lossy(), "StrictHostKeyChecking=no");
}

#[test]
fn parse_remote_shell_with_quoted_arguments() {
    use super::parse_remote_shell;

    let spec = OsStr::new("ssh -i '/path/with spaces/key' -p 2222");
    let args = parse_remote_shell(spec).expect("parsing should succeed");

    assert_eq!(args.len(), 5);
    assert_eq!(args[0].to_string_lossy(), "ssh");
    assert_eq!(args[1].to_string_lossy(), "-i");
    assert_eq!(args[2].to_string_lossy(), "/path/with spaces/key");
    assert_eq!(args[3].to_string_lossy(), "-p");
    assert_eq!(args[4].to_string_lossy(), "2222");
}

#[test]
fn parse_remote_shell_preserves_empty_strings() {
    use super::parse_remote_shell;

    let spec = OsStr::new("ssh -o 'Option='");
    let args = parse_remote_shell(spec).expect("parsing should succeed");

    assert_eq!(args.len(), 3);
    assert_eq!(args[0].to_string_lossy(), "ssh");
    assert_eq!(args[1].to_string_lossy(), "-o");
    assert_eq!(args[2].to_string_lossy(), "Option=");
}

#[test]
fn parse_remote_shell_handles_multiple_spaces() {
    use super::parse_remote_shell;

    let spec = OsStr::new("ssh   -p    2222");
    let args = parse_remote_shell(spec).expect("parsing should succeed");

    assert_eq!(args.len(), 3);
    assert_eq!(args[0].to_string_lossy(), "ssh");
    assert_eq!(args[1].to_string_lossy(), "-p");
    assert_eq!(args[2].to_string_lossy(), "2222");
}

#[cfg(unix)]
#[test]
fn split_connection_provides_separate_read_write_halves() {
    let connection = spawn_echo_process();

    let (mut reader, mut writer, child_handle) = connection.split().expect("split succeeds");

    // Write to stdin via writer
    writer.write_all(b"test").expect("write");
    writer.close().expect("close writer");

    // Read from stdout via reader
    let mut buffer = Vec::new();
    reader.read_to_end(&mut buffer).expect("read");
    assert_eq!(&buffer, b"test");

    // Wait for child via handle
    let status = child_handle.wait().expect("wait");
    assert!(status.success());
}

#[cfg(unix)]
#[test]
fn split_fails_after_stdin_closed() {
    let mut connection = spawn_echo_process();
    connection.close_stdin().expect("close stdin");

    let result = connection.split();
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::BrokenPipe);
}

#[cfg(unix)]
#[test]
fn try_wait_returns_none_while_running() {
    let mut connection = spawn_echo_process();

    let result = connection.try_wait().expect("try_wait");
    assert!(result.is_none(), "process should still be running");

    connection.close_stdin().expect("close stdin");
    let _ = connection.wait();
}

#[cfg(unix)]
#[test]
fn wait_returns_success_after_split() {
    let connection = spawn_echo_process();
    let (_reader, writer, handle) = connection.split().expect("split");

    writer.close().expect("close writer");
    let status = handle.wait().expect("wait via handle");
    assert!(status.success());
}

#[cfg(unix)]
#[test]
fn drop_child_handle_reaps_process() {
    let connection = spawn_echo_process();
    let (_reader, writer, child_handle) = connection.split().expect("split");

    // Close writer to signal EOF so the child exits naturally.
    writer.close().expect("close writer");

    // Drop the child handle WITHOUT calling wait().
    // The Drop impl should reap the process, preventing a zombie.
    drop(child_handle);
}

#[cfg(unix)]
#[test]
fn drop_child_handle_kills_running_process() {
    let mut command = SshCommand::new("ignored");
    command.set_program("sh");
    command.set_batch_mode(false);
    command.push_option("-c");
    command.push_option("sleep 60");

    let connection = command.spawn().expect("spawn shell");
    let (_reader, _writer, child_handle) = connection.split().expect("split");

    // Drop without waiting -- Drop should kill then reap.
    // If Drop didn't kill, this test would hang for 60 seconds.
    drop(child_handle);
}

#[test]
fn forces_aes_gcm_ciphers_when_explicitly_enabled() {
    let mut command = SshCommand::new("example.com");
    command.set_prefer_aes_gcm(Some(true));

    let (_, args) = command.command_parts_for_testing();
    let rendered = args_to_strings(&args);

    assert!(rendered.contains(&"-c".to_owned()));
    assert!(rendered.contains(&"aes128-gcm@openssh.com,aes256-gcm@openssh.com".to_owned()));
}

#[test]
fn omits_cipher_args_when_aes_explicitly_disabled() {
    let mut command = SshCommand::new("example.com");
    command.set_prefer_aes_gcm(Some(false));

    let (_, args) = command.command_parts_for_testing();
    let rendered = args_to_strings(&args);

    assert!(!rendered.contains(&"-c".to_owned()));
}

#[test]
fn auto_detects_aes_when_preference_is_none() {
    // Default (None) does not inject cipher args, preserving backward compatibility
    let command = SshCommand::new("example.com");
    let (_, args) = command.command_parts_for_testing();
    let rendered = args_to_strings(&args);

    assert!(!rendered.iter().any(|a| a == "-c"));
}

#[test]
fn skips_cipher_injection_when_custom_options_present() {
    // Auto-detect with custom options present -> no cipher injection
    let mut command = SshCommand::new("example.com");
    command.push_option("-v");

    let (_, args) = command.command_parts_for_testing();
    let rendered = args_to_strings(&args);

    assert!(!rendered.iter().any(|a| a == "-c"));
}

#[test]
fn skips_cipher_injection_for_non_ssh_program() {
    let mut command = SshCommand::new("example.com");
    command.set_program("plink");
    command.set_prefer_aes_gcm(Some(true));

    let (_, args) = command.command_parts_for_testing();
    let rendered = args_to_strings(&args);

    assert!(!rendered.contains(&"-c".to_owned()));
}

#[test]
fn injects_bind_address_ipv4() {
    let mut command = SshCommand::new("example.com");
    command.set_bind_address(Some(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100))));

    let (_, args) = command.command_parts_for_testing();
    let rendered = args_to_strings(&args);

    assert!(
        rendered.contains(&"-oBindAddress=192.168.1.100".to_owned()),
        "expected -oBindAddress=192.168.1.100 in {rendered:?}"
    );
}

#[test]
fn injects_bind_address_ipv6() {
    let mut command = SshCommand::new("example.com");
    command.set_bind_address(Some(IpAddr::V6(Ipv6Addr::LOCALHOST)));

    let (_, args) = command.command_parts_for_testing();
    let rendered = args_to_strings(&args);

    assert!(
        rendered.contains(&"-oBindAddress=::1".to_owned()),
        "expected -oBindAddress=::1 in {rendered:?}"
    );
}

#[test]
fn omits_bind_address_when_none() {
    let command = SshCommand::new("example.com");

    let (_, args) = command.command_parts_for_testing();
    let rendered = args_to_strings(&args);

    assert!(
        !rendered.iter().any(|a| a.starts_with("-oBindAddress=")),
        "bind address should not appear in {rendered:?}"
    );
}

#[test]
fn injects_keepalive_options_by_default() {
    let command = SshCommand::new("example.com");
    let (_, args) = command.command_parts_for_testing();
    let rendered = args_to_strings(&args);

    assert!(
        rendered.contains(&"-oServerAliveInterval=20".to_owned()),
        "expected ServerAliveInterval in {rendered:?}"
    );
    assert!(
        rendered.contains(&"-oServerAliveCountMax=3".to_owned()),
        "expected ServerAliveCountMax in {rendered:?}"
    );
}

#[test]
fn keepalive_options_appear_before_user_options() {
    let mut command = SshCommand::new("example.com");
    command.push_option("-v");

    let (_, args) = command.command_parts_for_testing();
    let rendered = args_to_strings(&args);

    let keepalive_pos = rendered
        .iter()
        .position(|a| a.starts_with("-oServerAliveInterval="))
        .expect("keepalive present");
    let option_pos = rendered.iter().position(|a| a == "-v").expect("-v present");

    assert!(
        keepalive_pos < option_pos,
        "keepalive at {keepalive_pos} should precede user option at {option_pos}"
    );
}

#[test]
fn omits_keepalive_when_explicitly_disabled() {
    let mut command = SshCommand::new("example.com");
    command.set_keepalive(false);

    let (_, args) = command.command_parts_for_testing();
    let rendered = args_to_strings(&args);

    assert!(
        !rendered.iter().any(|a| a.contains("ServerAlive")),
        "keepalive should not appear in {rendered:?}"
    );
}

#[test]
fn omits_keepalive_for_non_ssh_program() {
    let mut command = SshCommand::new("example.com");
    command.set_program("plink");

    let (_, args) = command.command_parts_for_testing();
    let rendered = args_to_strings(&args);

    assert!(
        !rendered.iter().any(|a| a.contains("ServerAlive")),
        "keepalive should not appear for non-ssh program in {rendered:?}"
    );
}

#[test]
fn skips_keepalive_when_user_specifies_server_alive_interval() {
    let mut command = SshCommand::new("example.com");
    command.push_option("-o");
    command.push_option("ServerAliveInterval=60");

    let (_, args) = command.command_parts_for_testing();
    let rendered = args_to_strings(&args);

    assert!(
        !rendered.iter().any(|a| a == "-oServerAliveInterval=20"),
        "should not inject default keepalive when user specified interval in {rendered:?}"
    );
    // The user's own option should still be present.
    assert!(
        rendered.contains(&"ServerAliveInterval=60".to_owned()),
        "user option should be preserved in {rendered:?}"
    );
}

#[test]
fn skips_keepalive_when_user_specifies_server_alive_count_max() {
    let mut command = SshCommand::new("example.com");
    command.push_option("-oServerAliveCountMax=5");

    let (_, args) = command.command_parts_for_testing();
    let rendered = args_to_strings(&args);

    assert!(
        !rendered.iter().any(|a| a == "-oServerAliveInterval=20"),
        "should not inject defaults when user specified count max in {rendered:?}"
    );
}

#[test]
fn skips_keepalive_when_remote_shell_contains_keepalive() {
    let mut command = SshCommand::new("example.com");
    command
        .configure_remote_shell(OsStr::new(
            "ssh -o ServerAliveInterval=30 -o ServerAliveCountMax=5",
        ))
        .expect("valid remote shell");

    let (_, args) = command.command_parts_for_testing();
    let rendered = args_to_strings(&args);

    assert!(
        !rendered.iter().any(|a| a == "-oServerAliveInterval=20"),
        "should not inject defaults when remote shell contains keepalive in {rendered:?}"
    );
}

#[test]
fn keepalive_detection_is_case_insensitive() {
    let mut command = SshCommand::new("example.com");
    command.push_option("-oserveraliveinterval=45");

    let (_, args) = command.command_parts_for_testing();
    let rendered = args_to_strings(&args);

    assert!(
        !rendered.iter().any(|a| a == "-oServerAliveInterval=20"),
        "case-insensitive detection should prevent injection in {rendered:?}"
    );
}

#[test]
fn keepalive_detection_is_case_insensitive_for_count_max() {
    let mut command = SshCommand::new("example.com");
    command.push_option("-oSERVERALIVECOUNTMAX=10");

    let (_, args) = command.command_parts_for_testing();
    let rendered = args_to_strings(&args);

    assert!(
        !rendered.iter().any(|a| a.contains("ServerAliveInterval")),
        "case-insensitive CountMax detection should prevent all keepalive injection in {rendered:?}"
    );
}

#[test]
fn omits_keepalive_for_rsh_program() {
    let mut command = SshCommand::new("example.com");
    command.set_program("rsh");

    let (_, args) = command.command_parts_for_testing();
    let rendered = args_to_strings(&args);

    assert!(
        !rendered.iter().any(|a| a.contains("ServerAlive")),
        "keepalive should not appear for rsh program in {rendered:?}"
    );
}

#[test]
fn omits_keepalive_for_absolute_path_non_ssh_program() {
    let mut command = SshCommand::new("example.com");
    command.set_program("/usr/local/bin/plink");

    let (_, args) = command.command_parts_for_testing();
    let rendered = args_to_strings(&args);

    assert!(
        !rendered.iter().any(|a| a.contains("ServerAlive")),
        "keepalive should not appear for non-ssh absolute path in {rendered:?}"
    );
}

#[test]
fn keepalive_injected_between_port_and_user_options() {
    let mut command = SshCommand::new("example.com");
    command.set_port(2222);
    command.push_option("-v");
    command.push_remote_arg("rsync");

    let (_, args) = command.command_parts_for_testing();
    let rendered = args_to_strings(&args);

    let port_pos = rendered.iter().position(|a| a == "-p").expect("-p present");
    let keepalive_pos = rendered
        .iter()
        .position(|a| a.starts_with("-oServerAliveInterval="))
        .expect("keepalive present");
    let user_opt_pos = rendered.iter().position(|a| a == "-v").expect("-v present");
    let target_pos = rendered
        .iter()
        .position(|a| a == "example.com")
        .expect("target present");
    let remote_pos = rendered
        .iter()
        .position(|a| a == "rsync")
        .expect("remote arg present");

    assert!(
        port_pos < keepalive_pos,
        "port at {port_pos} should precede keepalive at {keepalive_pos}"
    );
    assert!(
        keepalive_pos < user_opt_pos,
        "keepalive at {keepalive_pos} should precede user option at {user_opt_pos}"
    );
    assert!(
        user_opt_pos < target_pos,
        "user option at {user_opt_pos} should precede target at {target_pos}"
    );
    assert!(
        target_pos < remote_pos,
        "target at {target_pos} should precede remote arg at {remote_pos}"
    );
}

#[test]
fn keepalive_with_bind_address_and_batch_mode() {
    let mut command = SshCommand::new("example.com");
    command.set_bind_address(Some(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))));

    let (_, args) = command.command_parts_for_testing();
    let rendered = args_to_strings(&args);

    let batch_pos = rendered
        .iter()
        .position(|a| a == "-oBatchMode=yes")
        .expect("batch mode present");
    let bind_pos = rendered
        .iter()
        .position(|a| a.starts_with("-oBindAddress="))
        .expect("bind address present");
    let keepalive_pos = rendered
        .iter()
        .position(|a| a.starts_with("-oServerAliveInterval="))
        .expect("keepalive present");
    let target_pos = rendered
        .iter()
        .position(|a| a == "example.com")
        .expect("target present");

    assert!(
        batch_pos < bind_pos,
        "batch mode at {batch_pos} should precede bind address at {bind_pos}"
    );
    assert!(
        bind_pos < keepalive_pos,
        "bind address at {bind_pos} should precede keepalive at {keepalive_pos}"
    );
    assert!(
        keepalive_pos < target_pos,
        "keepalive at {keepalive_pos} should precede target at {target_pos}"
    );
}

#[cfg(unix)]
#[test]
fn stderr_drain_handles_non_utf8_output() {
    // Non-UTF-8 bytes on stderr must not terminate the drain thread prematurely.
    // This verifies the drain continues to collect all output even when
    // individual lines contain invalid UTF-8 sequences.
    let mut command = SshCommand::new("ignored");
    command.set_program("sh");
    command.set_batch_mode(false);
    command.push_option("-c");
    // Emit a valid line, then raw 0xFF bytes (invalid UTF-8), then another valid line.
    command.push_option(
        "printf 'before-binary\\n' >&2; printf '\\xff\\xfe\\n' >&2; printf 'after-binary\\n' >&2",
    );
    command.set_target_override(Some(OsString::new()));

    let connection = command.spawn().expect("spawn shell");
    let (_reader, _writer, child_handle) = connection.split().expect("split");

    let (status, stderr) = child_handle.wait_with_stderr().expect("wait");
    assert!(status.success());

    // Both the pre-binary and post-binary lines must be present,
    // proving the drain thread survived the non-UTF-8 bytes.
    let text = String::from_utf8_lossy(&stderr);
    assert!(
        text.contains("before-binary"),
        "expected 'before-binary' in stderr, got: {text}"
    );
    assert!(
        text.contains("after-binary"),
        "expected 'after-binary' after non-UTF-8 bytes, got: {text}"
    );
}

#[cfg(unix)]
#[test]
fn stderr_drain_forwards_multiline_errors() {
    // Verifies that multiple stderr lines are collected and forwarded,
    // simulating an SSH error like "Permission denied" followed by additional
    // context lines from the remote host.
    let mut command = SshCommand::new("ignored");
    command.set_program("sh");
    command.set_batch_mode(false);
    command.push_option("-c");
    command.push_option(
        "printf 'Permission denied (publickey).\\n' >&2; printf 'Connection closed by remote host\\n' >&2",
    );
    command.set_target_override(Some(OsString::new()));

    let connection = command.spawn().expect("spawn shell");
    let (_reader, _writer, child_handle) = connection.split().expect("split");

    let (status, stderr) = child_handle.wait_with_stderr().expect("wait");
    assert!(status.success());

    let text = String::from_utf8_lossy(&stderr);
    assert!(
        text.contains("Permission denied"),
        "expected 'Permission denied' in stderr, got: {text}"
    );
    assert!(
        text.contains("Connection closed"),
        "expected 'Connection closed' in stderr, got: {text}"
    );
}

#[cfg(unix)]
#[test]
fn stderr_output_available_on_connection_before_split() {
    // Verifies that stderr_output() works on the SshConnection itself
    // (not just SshChildHandle), since the drain thread starts at spawn time.
    use std::time::{Duration, Instant};

    let mut command = SshCommand::new("ignored");
    command.set_program("sh");
    command.set_batch_mode(false);
    command.push_option("-c");
    command.push_option("printf 'conn-level-msg\\n' >&2; cat");
    command.set_target_override(Some(OsString::new()));

    let connection = command.spawn().expect("spawn shell");

    // Poll stderr_output() on the connection (before split).
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let output = connection.stderr_output();
        if !output.is_empty() {
            let text = String::from_utf8_lossy(&output);
            assert!(
                text.contains("conn-level-msg"),
                "expected 'conn-level-msg', got: {text}"
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for stderr output on connection"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    let status = connection.wait().expect("wait");
    assert!(status.success());
}

#[test]
fn skips_keepalive_when_interval_embedded_in_compound_option() {
    // User passes keepalive as part of a compound `-o` value.
    let mut command = SshCommand::new("example.com");
    command.push_option("-oServerAliveInterval=120");

    let (_, args) = command.command_parts_for_testing();
    let rendered = args_to_strings(&args);

    assert!(
        !rendered.contains(&"-oServerAliveInterval=20".to_owned()),
        "should not inject default when user specified compound interval in {rendered:?}"
    );
    assert!(
        rendered.contains(&"-oServerAliveInterval=120".to_owned()),
        "user compound option should be preserved in {rendered:?}"
    );
}

#[test]
fn keepalive_re_enabled_after_disable() {
    let mut command = SshCommand::new("example.com");
    command.set_keepalive(false);
    command.set_keepalive(true);

    let (_, args) = command.command_parts_for_testing();
    let rendered = args_to_strings(&args);

    assert!(
        rendered.contains(&"-oServerAliveInterval=20".to_owned()),
        "re-enabling keepalive should inject options in {rendered:?}"
    );
    assert!(
        rendered.contains(&"-oServerAliveCountMax=3".to_owned()),
        "re-enabling keepalive should inject both options in {rendered:?}"
    );
}

#[test]
fn bind_address_appears_before_custom_options() {
    let mut command = SshCommand::new("example.com");
    command.set_bind_address(Some(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))));
    command.push_option("-v");

    let (_, args) = command.command_parts_for_testing();
    let rendered = args_to_strings(&args);

    let bind_pos = rendered
        .iter()
        .position(|a| a.starts_with("-oBindAddress="))
        .expect("bind address present");
    let option_pos = rendered.iter().position(|a| a == "-v").expect("-v present");

    assert!(
        bind_pos < option_pos,
        "bind address at {bind_pos} should precede custom option at {option_pos}"
    );
}

#[cfg(unix)]
#[test]
fn stderr_drain_forwards_error_output_after_split() {
    use std::time::Duration;

    // Spawn a child that writes to stderr and exits.
    let mut command = SshCommand::new("ignored");
    command.set_program("sh");
    command.set_batch_mode(false);
    command.push_option("-c");
    command.push_option("echo stderr-line >&2");

    // Use set_target_override to suppress the host argument.
    command.set_target_override(Some(OsString::new()));

    let connection = command.spawn().expect("spawn shell");
    let (_reader, _writer, child_handle) = connection.split().expect("split");

    // The drain thread is running in the background. Wait for the child to
    // exit so we know stderr has been fully written.
    let status = child_handle.wait().expect("wait");
    assert!(status.success());

    // We cannot easily capture eprintln! output in-process, but the fact that
    // the child exited successfully without deadlocking proves the drain works.
    // The thread joined cleanly inside wait().
    let _ = Duration::from_millis(0); // suppress unused import lint
}

#[cfg(unix)]
#[test]
fn stderr_drain_handles_large_output_without_deadlock() {
    // Write enough to stderr to exceed the OS pipe buffer (64 KB).
    // Without the drain thread, the child would block on the write and
    // never exit, causing this test to hang.
    let mut command = SshCommand::new("ignored");
    command.set_program("sh");
    command.set_batch_mode(false);
    command.push_option("-c");
    // Generate ~128 KB of stderr output (2048 lines of 64 bytes each).
    command.push_option(
        "i=0; while [ $i -lt 2048 ]; do printf 'AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\n' >&2; i=$((i+1)); done",
    );
    command.set_target_override(Some(OsString::new()));

    let connection = command.spawn().expect("spawn shell");
    let (_reader, _writer, child_handle) = connection.split().expect("split");

    // If the drain thread is not running, the child blocks on stderr write
    // and this wait() never returns.
    let status = child_handle.wait().expect("wait");
    assert!(status.success());
}

#[cfg(unix)]
#[test]
fn stderr_drain_joins_on_drop() {
    let mut command = SshCommand::new("ignored");
    command.set_program("sh");
    command.set_batch_mode(false);
    command.push_option("-c");
    command.push_option("echo drop-test >&2");
    command.set_target_override(Some(OsString::new()));

    let connection = command.spawn().expect("spawn shell");
    let (_reader, writer, child_handle) = connection.split().expect("split");

    // Close writer so child can exit.
    writer.close().expect("close writer");

    // Drop without calling wait() - the drain thread should be joined by
    // StderrDrain's Drop impl, and the child reaped by SshChildHandle's Drop.
    drop(child_handle);
}

#[cfg(unix)]
#[test]
fn stderr_deadlock_regression_large_stderr_does_not_block_stdout() {
    // Regression test: when a child writes large amounts to stderr while also
    // writing to stdout, the parent must be able to read stdout without
    // deadlocking. Without the StderrDrain background thread, the child would
    // fill the OS pipe buffer (~64 KB) on stderr, block, and never write to
    // stdout - causing the parent to block on stdout read indefinitely.
    //
    // This test uses a 5-second timeout to detect deadlock rather than
    // hanging the test suite.
    use std::sync::mpsc;
    use std::time::Duration;

    let mut command = SshCommand::new("ignored");
    command.set_program("sh");
    command.set_batch_mode(false);
    command.push_option("-c");
    // Write ~640 KB to stderr (10000 lines of ~64 bytes) interleaved with
    // a known sentinel on stdout. The stderr volume far exceeds the OS pipe
    // buffer. Without draining, the child blocks on stderr before reaching
    // the stdout write.
    command.push_option(concat!(
        "i=0; while [ $i -lt 10000 ]; do ",
        "printf 'error: this is a long stderr line that helps fill the pipe buffer quickly\\n' >&2; ",
        "i=$((i+1)); done; ",
        "printf 'STDOUT_SENTINEL'"
    ));
    command.set_target_override(Some(std::ffi::OsString::new()));

    let connection = command.spawn().expect("spawn shell");
    let (mut reader, _writer, child_handle) = connection.split().expect("split");

    // Read stdout on a separate thread so we can enforce a timeout.
    let (tx, rx) = mpsc::channel();
    let read_thread = std::thread::Builder::new()
        .name("test-stdout-reader".into())
        .spawn(move || {
            let mut buf = Vec::new();
            let result = reader.read_to_end(&mut buf);
            let _ = tx.send((result, buf));
        })
        .expect("spawn reader thread");

    // 5-second timeout: if the drain is broken, the child blocks on stderr
    // and never writes the sentinel to stdout, so this recv times out.
    let (result, buf) = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("stdout read should complete within 5 seconds (deadlock detected)");

    result.expect("stdout read should succeed");
    let output = String::from_utf8_lossy(&buf);
    assert!(
        output.contains("STDOUT_SENTINEL"),
        "expected sentinel in stdout, got: {output:?}"
    );

    let status = child_handle.wait().expect("wait");
    assert!(status.success());
    read_thread.join().expect("reader thread panicked");
}

#[cfg(unix)]
#[test]
fn stderr_drain_with_no_stderr_output() {
    // Child produces no stderr output - drain thread should exit cleanly at EOF.
    let mut command = SshCommand::new("ignored");
    command.set_program("sh");
    command.set_batch_mode(false);
    command.push_option("-c");
    command.push_option("echo stdout-only");
    command.set_target_override(Some(OsString::new()));

    let connection = command.spawn().expect("spawn shell");
    let (_reader, _writer, child_handle) = connection.split().expect("split");

    let status = child_handle.wait().expect("wait");
    assert!(status.success());
}

#[cfg(unix)]
#[test]
fn connection_wait_with_stderr_captures_error_on_failure() {
    // Verifies that when an SSH-like subprocess fails (non-zero exit), the
    // stderr message is captured and returned to the caller via
    // `SshConnection::wait_with_stderr()`. This is the primary path for
    // surfacing SSH connection errors (e.g., "Permission denied") to the user.
    let mut command = SshCommand::new("ignored");
    command.set_program("sh");
    command.set_batch_mode(false);
    command.push_option("-c");
    command.push_option(
        "printf 'ssh: connect to host unreachable port 22: Connection refused\\n' >&2; exit 255",
    );
    command.set_target_override(Some(OsString::new()));

    let connection = command.spawn().expect("spawn shell");
    let (status, stderr) = connection.wait_with_stderr().expect("wait");

    assert!(!status.success(), "child should exit with non-zero status");
    assert_eq!(
        status.code(),
        Some(255),
        "exit code should be 255 (SSH connection failure)"
    );

    let text = String::from_utf8_lossy(&stderr);
    assert!(
        text.contains("Connection refused"),
        "stderr should contain the SSH error message, got: {text}"
    );
}

#[cfg(unix)]
#[test]
fn connection_wait_with_stderr_empty_on_success() {
    // Verifies that wait_with_stderr returns empty stderr on successful exit.
    let connection = spawn_echo_process();
    let (status, stderr) = connection.wait_with_stderr().expect("wait");

    assert!(status.success());
    assert!(
        stderr.is_empty(),
        "expected empty stderr on success, got: {stderr:?}"
    );
}

#[cfg(unix)]
#[test]
fn child_handle_wait_with_stderr_surfaces_error_after_split() {
    // Verifies the split() path also captures stderr on connection failure.
    // This covers the path used by ssh_transfer.rs and remote_to_remote.rs.
    let mut command = SshCommand::new("ignored");
    command.set_program("sh");
    command.set_batch_mode(false);
    command.push_option("-c");
    command.push_option("printf 'Permission denied (publickey).\\n' >&2; exit 255");
    command.set_target_override(Some(OsString::new()));

    let connection = command.spawn().expect("spawn shell");
    let (_reader, _writer, child_handle) = connection.split().expect("split");

    let (status, stderr) = child_handle.wait_with_stderr().expect("wait");

    assert!(!status.success());
    let text = String::from_utf8_lossy(&stderr);
    assert!(
        text.contains("Permission denied"),
        "stderr should contain the SSH error message, got: {text}"
    );
}

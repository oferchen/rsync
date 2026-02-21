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
        vec!["-oBatchMode=yes".to_owned(), "example.com".to_owned()]
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
    assert_eq!(args_to_strings(&args), vec!["example.com".to_owned()]);
}

#[test]
fn wraps_ipv6_hosts_in_brackets() {
    let command = SshCommand::new("2001:db8::1");
    let (_, args) = command.command_parts_for_testing();

    assert_eq!(
        args_to_strings(&args),
        vec!["-oBatchMode=yes".to_owned(), "[2001:db8::1]".to_owned()]
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
            "backup@[2001:db8::1]".to_owned()
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
            "backup@[2001:db8::1]".to_owned()
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
        vec!["-oBatchMode=yes".to_owned(), "rsync".to_owned()]
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
        vec!["-oBatchMode=yes".to_owned(), "custom-target".to_owned()]
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
        vec!["-oBatchMode=yes".to_owned(), "rsync".to_owned()]
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
fn stderr_stream_is_accessible() {
    let mut command = SshCommand::new("ignored");
    command.set_program("sh");
    command.set_batch_mode(false);
    command.push_option("-c");
    command.push_option("printf err >&2");

    let mut connection = command.spawn().expect("spawn shell");
    connection.close_stdin().expect("close stdin");

    let mut stderr = String::new();
    connection
        .stderr_mut()
        .expect("stderr handle")
        .read_to_string(&mut stderr)
        .expect("read stderr");
    assert!(stderr.contains("err"));

    let status = connection.wait().expect("wait status");
    assert!(status.success());
}

#[cfg(unix)]
#[test]
fn take_stderr_transfers_handle() {
    let mut command = SshCommand::new("ignored");
    command.set_program("sh");
    command.set_batch_mode(false);
    command.push_option("-c");
    command.push_option("printf err >&2");

    let mut connection = command.spawn().expect("spawn shell");
    connection.close_stdin().expect("close stdin");

    let mut handle = connection.take_stderr().expect("stderr handle");
    assert!(connection.take_stderr().is_none());

    let mut captured = String::new();
    handle.read_to_string(&mut captured).expect("read stderr");
    assert!(captured.contains("err"));

    let status = connection.wait().expect("wait status");
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

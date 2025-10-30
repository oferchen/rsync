use super::{SshCommand, SshConnection};
use std::ffi::{OsStr, OsString};
use std::io::{Read, Write};

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
        vec!["-oBatchMode=yes".to_string(), "example.com".to_string()]
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
            "-oBatchMode=yes".to_string(),
            "-p".to_string(),
            "2222".to_string(),
            "-vvv".to_string(),
            "backup@rsync.example.com".to_string(),
            "rsync".to_string(),
            "--server".to_string(),
            ".".to_string(),
        ]
    );
}

#[test]
fn disables_batch_mode_when_requested() {
    let mut command = SshCommand::new("example.com");
    command.set_batch_mode(false);

    let (_, args) = command.command_parts_for_testing();
    assert_eq!(args_to_strings(&args), vec!["example.com".to_string()]);
}

#[test]
fn wraps_ipv6_hosts_in_brackets() {
    let command = SshCommand::new("2001:db8::1");
    let (_, args) = command.command_parts_for_testing();

    assert_eq!(
        args_to_strings(&args),
        vec!["-oBatchMode=yes".to_string(), "[2001:db8::1]".to_string()]
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
            "-oBatchMode=yes".to_string(),
            "backup@[2001:db8::1]".to_string()
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
            "-oBatchMode=yes".to_string(),
            "backup@[2001:db8::1]".to_string()
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
        vec!["-oBatchMode=yes".to_string(), "rsync".to_string()]
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
        vec!["-oBatchMode=yes".to_string(), "custom-target".to_string()]
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
        vec!["-oBatchMode=yes".to_string(), "rsync".to_string()]
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

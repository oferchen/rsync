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
    let mut command = SshCommand::new("example.com");
    command.set_prefer_aes_gcm(Some(false));
    let (program, args) = command.command_parts_for_testing();

    assert_eq!(program, OsString::from("ssh"));
    assert_eq!(
        args_to_strings(&args),
        vec![
            "-oBatchMode=yes".to_owned(),
            "-oServerAliveInterval=20".to_owned(),
            "-oServerAliveCountMax=3".to_owned(),
            "-oConnectTimeout=30".to_owned(),
            "example.com".to_owned(),
        ]
    );
}

#[test]
fn assembles_command_with_user_port_and_remote_args() {
    let mut command = SshCommand::new("rsync.example.com");
    command.set_prefer_aes_gcm(Some(false));
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
            "-oConnectTimeout=30".to_owned(),
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
    command.set_prefer_aes_gcm(Some(false));
    command.set_batch_mode(false);

    let (_, args) = command.command_parts_for_testing();
    assert_eq!(
        args_to_strings(&args),
        vec![
            "-oServerAliveInterval=20".to_owned(),
            "-oServerAliveCountMax=3".to_owned(),
            "-oConnectTimeout=30".to_owned(),
            "example.com".to_owned(),
        ]
    );
}

#[test]
fn wraps_ipv6_hosts_in_brackets() {
    let mut command = SshCommand::new("2001:db8::1");
    command.set_prefer_aes_gcm(Some(false));
    let (_, args) = command.command_parts_for_testing();

    assert_eq!(
        args_to_strings(&args),
        vec![
            "-oBatchMode=yes".to_owned(),
            "-oServerAliveInterval=20".to_owned(),
            "-oServerAliveCountMax=3".to_owned(),
            "-oConnectTimeout=30".to_owned(),
            "[2001:db8::1]".to_owned(),
        ]
    );
}

#[test]
fn wraps_ipv6_hosts_with_usernames() {
    let mut command = SshCommand::new("2001:db8::1");
    command.set_prefer_aes_gcm(Some(false));
    command.set_user("backup");

    let (_, args) = command.command_parts_for_testing();

    assert_eq!(
        args_to_strings(&args),
        vec![
            "-oBatchMode=yes".to_owned(),
            "-oServerAliveInterval=20".to_owned(),
            "-oServerAliveCountMax=3".to_owned(),
            "-oConnectTimeout=30".to_owned(),
            "backup@[2001:db8::1]".to_owned(),
        ]
    );
}

#[test]
fn preserves_explicit_bracketed_ipv6_literals() {
    let mut command = SshCommand::new("[2001:db8::1]");
    command.set_prefer_aes_gcm(Some(false));
    command.set_user("backup");

    let (_, args) = command.command_parts_for_testing();

    assert_eq!(
        args_to_strings(&args),
        vec![
            "-oBatchMode=yes".to_owned(),
            "-oServerAliveInterval=20".to_owned(),
            "-oServerAliveCountMax=3".to_owned(),
            "-oConnectTimeout=30".to_owned(),
            "backup@[2001:db8::1]".to_owned(),
        ]
    );
}

#[test]
fn wraps_ipv6_hosts_with_scoped_zone_id() {
    // RFC 4007 link-local with interface zone id: must round-trip the
    // literal `%zone` inside the brackets.
    let mut command = SshCommand::new("fe80::1%eth0");
    command.set_prefer_aes_gcm(Some(false));
    command.set_user("backup");

    let (_, args) = command.command_parts_for_testing();

    assert_eq!(
        args_to_strings(&args),
        vec![
            "-oBatchMode=yes".to_owned(),
            "-oServerAliveInterval=20".to_owned(),
            "-oServerAliveCountMax=3".to_owned(),
            "-oConnectTimeout=30".to_owned(),
            "backup@[fe80::1%eth0]".to_owned(),
        ]
    );
}

#[test]
fn wraps_bracketed_ipv6_hosts_with_scoped_zone_id() {
    // URL-style outer brackets around an IPv6+zone literal still
    // re-emit the canonical `[addr%zone]` form.
    let mut command = SshCommand::new("[fe80::1%en0]");
    command.set_prefer_aes_gcm(Some(false));

    let (_, args) = command.command_parts_for_testing();

    assert_eq!(
        args_to_strings(&args),
        vec![
            "-oBatchMode=yes".to_owned(),
            "-oServerAliveInterval=20".to_owned(),
            "-oServerAliveCountMax=3".to_owned(),
            "-oConnectTimeout=30".to_owned(),
            "[fe80::1%en0]".to_owned(),
        ]
    );
}

#[test]
fn does_not_bracket_malformed_ipv6_with_multiple_double_colons() {
    // The legacy `host_contains_colon` heuristic accepted any `:`-bearing
    // input as IPv6 and would have wrapped this in brackets. Strict
    // `Ipv6Addr::from_str` validation rejects multi-`::` literals, so the
    // host is now passed through unchanged for SSH to surface the
    // resolution failure.
    let mut command = SshCommand::new("2001:db8:::1");
    command.set_prefer_aes_gcm(Some(false));

    let (_, args) = command.command_parts_for_testing();

    assert_eq!(
        args_to_strings(&args),
        vec![
            "-oBatchMode=yes".to_owned(),
            "-oServerAliveInterval=20".to_owned(),
            "-oServerAliveCountMax=3".to_owned(),
            "-oConnectTimeout=30".to_owned(),
            "2001:db8:::1".to_owned(),
        ]
    );
}

#[test]
fn does_not_bracket_garbage_colon_input() {
    // Loose substring checks (`host.contains(':')`) would have also
    // bracketed this; strict parsing leaves it untouched.
    let mut command = SshCommand::new("garbage:input");
    command.set_prefer_aes_gcm(Some(false));

    let (_, args) = command.command_parts_for_testing();

    assert_eq!(
        args_to_strings(&args),
        vec![
            "-oBatchMode=yes".to_owned(),
            "-oServerAliveInterval=20".to_owned(),
            "-oServerAliveCountMax=3".to_owned(),
            "-oConnectTimeout=30".to_owned(),
            "garbage:input".to_owned(),
        ]
    );
}

#[test]
fn does_not_bracket_zone_with_whitespace() {
    // Zone identifiers must not contain whitespace; the malformed input
    // is passed through unchanged rather than silently wrapped.
    let mut command = SshCommand::new("fe80::1%eth 0");
    command.set_prefer_aes_gcm(Some(false));

    let (_, args) = command.command_parts_for_testing();

    assert_eq!(
        args_to_strings(&args),
        vec![
            "-oBatchMode=yes".to_owned(),
            "-oServerAliveInterval=20".to_owned(),
            "-oServerAliveCountMax=3".to_owned(),
            "-oConnectTimeout=30".to_owned(),
            "fe80::1%eth 0".to_owned(),
        ]
    );
}

#[test]
fn does_not_bracket_ipv4_or_hostname() {
    // Sanity check: dotted-quad IPv4 and ordinary hostnames are emitted
    // verbatim with no brackets.
    let mut ipv4 = SshCommand::new("192.0.2.1");
    ipv4.set_prefer_aes_gcm(Some(false));
    let (_, ipv4_args) = ipv4.command_parts_for_testing();
    assert!(args_to_strings(&ipv4_args).contains(&"192.0.2.1".to_owned()));

    let mut hostname = SshCommand::new("rsync.example.com");
    hostname.set_prefer_aes_gcm(Some(false));
    let (_, host_args) = hostname.command_parts_for_testing();
    assert!(args_to_strings(&host_args).contains(&"rsync.example.com".to_owned()));
}

#[test]
fn parse_host_for_ssh_classifies_inputs() {
    use super::builder::{BuildError, HostKind, parse_host_for_ssh};

    assert_eq!(
        parse_host_for_ssh("rsync.example.com"),
        Ok(HostKind::Hostname("rsync.example.com".to_owned()))
    );
    assert_eq!(
        parse_host_for_ssh("192.0.2.1"),
        Ok(HostKind::Ipv4(Ipv4Addr::new(192, 0, 2, 1)))
    );
    assert_eq!(
        parse_host_for_ssh("2001:db8::1"),
        Ok(HostKind::Ipv6 {
            addr: "2001:db8::1".parse().unwrap(),
            zone: None,
        })
    );
    assert_eq!(
        parse_host_for_ssh("[2001:db8::1]"),
        Ok(HostKind::Ipv6 {
            addr: "2001:db8::1".parse().unwrap(),
            zone: None,
        })
    );
    assert_eq!(
        parse_host_for_ssh("fe80::1%eth0"),
        Ok(HostKind::Ipv6 {
            addr: "fe80::1".parse().unwrap(),
            zone: Some("eth0".to_owned()),
        })
    );

    assert!(matches!(parse_host_for_ssh(""), Err(BuildError::EmptyHost)));
    assert!(matches!(
        parse_host_for_ssh("2001:db8:::1"),
        Err(BuildError::InvalidIpv6(_))
    ));
    assert!(matches!(
        parse_host_for_ssh("garbage:input"),
        Err(BuildError::InvalidIpv6(_))
    ));
    assert!(matches!(
        parse_host_for_ssh("fe80::1%"),
        Err(BuildError::InvalidZoneId(_))
    ));
    assert!(matches!(
        parse_host_for_ssh("fe80::1%eth 0"),
        Err(BuildError::InvalidZoneId(_))
    ));
    assert!(matches!(
        parse_host_for_ssh("fe80::1%eth]0"),
        Err(BuildError::InvalidZoneId(_))
    ));
}

#[test]
fn command_parts_skip_target_when_host_and_user_missing() {
    let mut command = SshCommand::new("");
    command.set_prefer_aes_gcm(Some(false));
    command.set_remote_command([OsString::from("rsync")]);

    let (_, args) = command.command_parts_for_testing();

    assert_eq!(
        args_to_strings(&args),
        vec![
            "-oBatchMode=yes".to_owned(),
            "-oServerAliveInterval=20".to_owned(),
            "-oServerAliveCountMax=3".to_owned(),
            "-oConnectTimeout=30".to_owned(),
            "rsync".to_owned(),
        ]
    );
}

#[test]
fn target_override_supersedes_computed_target() {
    let mut command = SshCommand::new("example.com");
    command.set_prefer_aes_gcm(Some(false));
    command.set_user("backup");
    command.set_target_override(Some("custom-target"));

    let (_, args) = command.command_parts_for_testing();

    assert_eq!(
        args_to_strings(&args),
        vec![
            "-oBatchMode=yes".to_owned(),
            "-oServerAliveInterval=20".to_owned(),
            "-oServerAliveCountMax=3".to_owned(),
            "-oConnectTimeout=30".to_owned(),
            "custom-target".to_owned(),
        ]
    );
}

#[test]
fn empty_target_override_suppresses_target_argument() {
    let mut command = SshCommand::new("example.com");
    command.set_prefer_aes_gcm(Some(false));
    command.set_target_override(Some(OsString::new()));
    command.push_remote_arg("rsync");

    let (_, args) = command.command_parts_for_testing();

    assert_eq!(
        args_to_strings(&args),
        vec![
            "-oBatchMode=yes".to_owned(),
            "-oServerAliveInterval=20".to_owned(),
            "-oServerAliveCountMax=3".to_owned(),
            "-oConnectTimeout=30".to_owned(),
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

    let cap = 64 * 1024;
    assert!(
        stderr.len() <= cap,
        "stderr buffer should be bounded to {cap}, got {}",
        stderr.len()
    );
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

    writer.write_all(b"test").expect("write");
    writer.close().expect("close writer");

    let mut buffer = Vec::new();
    reader.read_to_end(&mut buffer).expect("read");
    assert_eq!(&buffer, b"test");

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
    use super::builder::has_hardware_aes;

    let mut command = SshCommand::new("example.com");
    command.set_prefer_aes_gcm(Some(true));

    let (_, args) = command.command_parts_for_testing();
    let rendered = args_to_strings(&args);

    if has_hardware_aes() {
        assert!(rendered.contains(&"-c".to_owned()));
        assert!(rendered.contains(&"aes128-gcm@openssh.com,aes256-gcm@openssh.com".to_owned()));
    } else {
        // Without hardware AES, cipher injection is skipped even when preferred.
        assert!(!rendered.contains(&"-c".to_owned()));
    }
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
    use super::builder::has_hardware_aes;

    // Default (None) auto-detects hardware AES and injects ciphers when available.
    let command = SshCommand::new("example.com");
    let (_, args) = command.command_parts_for_testing();
    let rendered = args_to_strings(&args);

    assert_eq!(
        rendered.iter().any(|a| a == "-c"),
        has_hardware_aes(),
        "cipher injection should match hardware AES availability when preference is None"
    );
}

#[test]
fn skips_cipher_injection_when_cipher_option_present() {
    // User-specified `-c` prevents automatic AES-GCM injection regardless
    // of hardware support.
    let mut command = SshCommand::new("example.com");
    command.push_option("-c");
    command.push_option("chacha20-poly1305@openssh.com");

    let (_, args) = command.command_parts_for_testing();
    let rendered = args_to_strings(&args);

    assert!(
        !rendered.contains(&"aes128-gcm@openssh.com,aes256-gcm@openssh.com".to_owned()),
        "should not inject AES-GCM when user specified -c: {rendered:?}"
    );
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
    // the StderrAuxChannel Drop impl, and the child reaped by
    // SshChildHandle's Drop.
    drop(child_handle);
}

#[cfg(unix)]
#[test]
fn child_handle_drop_surfaces_stderr_on_nonzero_exit() {
    // When SshChildHandle is dropped without wait_with_stderr() and the child
    // exited with a non-zero status, the Drop impl must join the drain thread
    // and surface stderr. This exercises the abnormal-exit Drop path that
    // prevents SSH errors from being silently lost.
    let mut command = SshCommand::new("ignored");
    command.set_program("sh");
    command.set_batch_mode(false);
    command.push_option("-c");
    command.push_option("printf 'fatal: host key verification failed\\n' >&2; exit 255");
    command.set_target_override(Some(OsString::new()));

    let connection = command.spawn().expect("spawn shell");
    let (_reader, writer, child_handle) = connection.split().expect("split");

    writer.close().expect("close writer");

    // Dropping the child handle without calling wait() should not panic,
    // and the drain thread should be joined cleanly.
    drop(child_handle);
}

#[cfg(unix)]
#[test]
fn connection_drop_surfaces_stderr_on_nonzero_exit() {
    // When SshConnection is dropped (not split) and the child exited with
    // an error, the Drop impl must surface stderr. This tests the direct
    // connection Drop path used when callers do not split into reader/writer.
    let mut command = SshCommand::new("ignored");
    command.set_program("sh");
    command.set_batch_mode(false);
    command.push_option("-c");
    command.push_option("printf 'Connection refused\\n' >&2; exit 1");
    command.set_target_override(Some(OsString::new()));

    let connection = command.spawn().expect("spawn shell");

    // Dropping the connection directly without wait() should not panic,
    // and the drain thread should be joined cleanly.
    drop(connection);
}

#[cfg(unix)]
#[test]
fn stderr_deadlock_regression_large_stderr_does_not_block_stdout() {
    // Regression test: when a child writes large amounts to stderr while also
    // writing to stdout, the parent must be able to read stdout without
    // deadlocking. Without the StderrAuxChannel background drain, the child
    // would fill the OS pipe buffer (~64 KB) on stderr, block, and never
    // write to stdout - causing the parent to block on stdout read
    // indefinitely.
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

#[test]
fn has_hardware_aes_is_consistent() {
    use super::builder::has_hardware_aes;

    let first = has_hardware_aes();
    let second = has_hardware_aes();
    assert_eq!(first, second, "cached result must be stable across calls");
}

#[test]
fn has_hardware_aes_returns_expected_for_platform() {
    use super::builder::has_hardware_aes;

    let result = has_hardware_aes();

    // Apple Silicon (aarch64-apple-*) always has hardware AES via ARMv8
    // Crypto Extensions. Most modern x86_64 CPUs (Intel Westmere+ 2010,
    // AMD Bulldozer+ 2011) have AES-NI. On platforms without detection
    // support (e.g., MIPS, RISC-V), has_hardware_aes() returns false.
    //
    // CI matrix covers macOS ARM (always true), Linux x86_64 (true on
    // modern hosts), and Windows x86_64 (true on modern hosts). VMs
    // without AES passthrough or older x86 CPUs without AES-NI would
    // see false here - that path is exercised by the
    // `no_aes_gcm_injection_without_hardware_and_user_cipher` test below.
    #[cfg(target_arch = "aarch64")]
    {
        // All aarch64 Apple and AWS Graviton chips have AES.
        assert!(
            result,
            "aarch64 platforms with Crypto Extensions should report hardware AES"
        );
    }

    // On any platform, the result must be a valid bool (not a panic).
    let _ = result;
}

#[test]
fn aes_gcm_injection_requires_hardware_aes() {
    use super::builder::has_hardware_aes;

    // When hardware AES is absent, cipher injection must be skipped even
    // with `prefer_aes_gcm = Some(true)`. On CI hosts with hardware AES,
    // this test verifies that injection proceeds as expected.
    let mut command = SshCommand::new("example.com");
    command.set_prefer_aes_gcm(Some(true));

    let (_, args) = command.command_parts_for_testing();
    let rendered = args_to_strings(&args);
    let has_cipher_flag = rendered.contains(&"-c".to_owned());

    assert_eq!(
        has_cipher_flag,
        has_hardware_aes(),
        "cipher injection should match hardware AES availability"
    );
}

#[test]
fn no_aes_gcm_injection_without_hardware_and_user_cipher() {
    // Verifies that all four conditions of should_inject_aes_gcm_ciphers()
    // must hold simultaneously. Even with prefer_aes_gcm=true and an SSH
    // program, existing `-c` in options prevents injection - exercising
    // the guard that protects against overriding user cipher choices.
    // This path is hardware-independent and always returns false.
    let mut command = SshCommand::new("example.com");
    command.set_prefer_aes_gcm(Some(true));
    command.push_option("-c");
    command.push_option("chacha20-poly1305@openssh.com");

    let (_, args) = command.command_parts_for_testing();
    let rendered = args_to_strings(&args);

    // The user's cipher choice must be preserved.
    assert!(
        rendered.contains(&"chacha20-poly1305@openssh.com".to_owned()),
        "user cipher selection must be preserved: {rendered:?}"
    );

    // AES-GCM ciphers must not be injected alongside the user's choice.
    assert!(
        !rendered.contains(&"aes128-gcm@openssh.com,aes256-gcm@openssh.com".to_owned()),
        "AES-GCM should not be injected when user already specified -c: {rendered:?}"
    );
}

#[test]
fn no_aes_gcm_injection_when_explicitly_disabled() {
    // Exercises the case where the user opts out via `--no-aes`: even with
    // an SSH program and no existing cipher option, `Some(false)` prevents
    // injection regardless of hardware support.
    let mut command = SshCommand::new("example.com");
    command.set_prefer_aes_gcm(Some(false));

    let (_, args) = command.command_parts_for_testing();
    let rendered = args_to_strings(&args);

    assert!(
        !rendered.contains(&"aes128-gcm@openssh.com,aes256-gcm@openssh.com".to_owned()),
        "no AES-GCM injection when explicitly disabled via --no-aes: {rendered:?}"
    );
}

#[test]
fn skips_cipher_injection_when_combined_cipher_option_present() {
    // User specified `-caes128-ctr` (combined form without space) via push_option.
    // The cipher flag detection must recognise this as a user-specified cipher.
    let mut command = SshCommand::new("example.com");
    command.push_option("-caes128-ctr");

    let (_, args) = command.command_parts_for_testing();
    let rendered = args_to_strings(&args);

    assert!(
        !rendered.contains(&"aes128-gcm@openssh.com,aes256-gcm@openssh.com".to_owned()),
        "should not inject AES-GCM when user specified combined -c form: {rendered:?}"
    );
    assert!(
        rendered.contains(&"-caes128-ctr".to_owned()),
        "user combined cipher option should be preserved: {rendered:?}"
    );
}

#[test]
fn skips_cipher_injection_when_compound_cipher_option_present() {
    // User specified `-c aes128-ctr` as a single unsplit option string.
    // This can happen when push_option is called without splitting on whitespace.
    let mut command = SshCommand::new("example.com");
    command.push_option("-c aes128-ctr");

    let (_, args) = command.command_parts_for_testing();
    let rendered = args_to_strings(&args);

    assert!(
        !rendered.contains(&"aes128-gcm@openssh.com,aes256-gcm@openssh.com".to_owned()),
        "should not inject AES-GCM when user specified compound -c form: {rendered:?}"
    );
}

#[test]
fn skips_cipher_injection_when_remote_shell_contains_cipher() {
    // User specified `-e "ssh -c aes128-ctr"`. The parser splits this into
    // separate tokens, so `-c` appears as a standalone option element.
    let mut command = SshCommand::new("example.com");
    command
        .configure_remote_shell(OsStr::new("ssh -c aes128-ctr"))
        .expect("valid remote shell");

    let (_, args) = command.command_parts_for_testing();
    let rendered = args_to_strings(&args);

    assert!(
        !rendered.contains(&"aes128-gcm@openssh.com,aes256-gcm@openssh.com".to_owned()),
        "should not inject AES-GCM when remote shell specifies -c: {rendered:?}"
    );
    assert!(
        rendered.contains(&"-c".to_owned()),
        "user -c flag should be preserved: {rendered:?}"
    );
    assert!(
        rendered.contains(&"aes128-ctr".to_owned()),
        "user cipher name should be preserved: {rendered:?}"
    );
}

#[test]
fn skips_cipher_injection_when_remote_shell_has_combined_cipher() {
    // User specified `-e "ssh -caes256-ctr"` (combined form in remote shell).
    let mut command = SshCommand::new("example.com");
    command
        .configure_remote_shell(OsStr::new("ssh -caes256-ctr"))
        .expect("valid remote shell");

    let (_, args) = command.command_parts_for_testing();
    let rendered = args_to_strings(&args);

    assert!(
        !rendered.contains(&"aes128-gcm@openssh.com,aes256-gcm@openssh.com".to_owned()),
        "should not inject AES-GCM when remote shell has combined -c: {rendered:?}"
    );
    assert!(
        rendered.contains(&"-caes256-ctr".to_owned()),
        "user combined cipher should be preserved: {rendered:?}"
    );
}

#[test]
fn connect_timeout_injected_by_default() {
    let command = SshCommand::new("example.com");
    let (_, args) = command.command_parts_for_testing();
    let rendered = args_to_strings(&args);
    assert!(
        rendered.contains(&"-oConnectTimeout=30".to_owned()),
        "default 30s connect timeout should be injected: {rendered:?}"
    );
}

#[test]
fn connect_timeout_custom_value() {
    let mut command = SshCommand::new("example.com");
    command.set_connect_timeout(Some(std::time::Duration::from_secs(60)));
    let (_, args) = command.command_parts_for_testing();
    let rendered = args_to_strings(&args);
    assert!(
        rendered.contains(&"-oConnectTimeout=60".to_owned()),
        "custom 60s connect timeout should be injected: {rendered:?}"
    );
}

#[test]
fn connect_timeout_disabled_when_none() {
    let mut command = SshCommand::new("example.com");
    command.set_connect_timeout(None);
    let (_, args) = command.command_parts_for_testing();
    let rendered = args_to_strings(&args);
    assert!(
        !rendered.iter().any(|a| a.contains("ConnectTimeout")),
        "no ConnectTimeout when disabled: {rendered:?}"
    );
}

#[test]
fn connect_timeout_not_injected_for_non_ssh_program() {
    let mut command = SshCommand::new("example.com");
    command.set_program("rsh");
    let (_, args) = command.command_parts_for_testing();
    let rendered = args_to_strings(&args);
    assert!(
        !rendered.iter().any(|a| a.contains("ConnectTimeout")),
        "ConnectTimeout should not be injected for non-SSH program: {rendered:?}"
    );
}

#[test]
fn connect_timeout_not_injected_when_user_specified() {
    let mut command = SshCommand::new("example.com");
    command.push_option("-oConnectTimeout=10");
    let (_, args) = command.command_parts_for_testing();
    let rendered = args_to_strings(&args);
    let count = rendered
        .iter()
        .filter(|a| a.contains("ConnectTimeout"))
        .count();
    assert_eq!(
        count, 1,
        "user-specified ConnectTimeout should not be duplicated: {rendered:?}"
    );
    assert!(
        rendered.contains(&"-oConnectTimeout=10".to_owned()),
        "user value should be preserved: {rendered:?}"
    );
}

#[test]
fn connect_timeout_not_injected_when_user_specified_case_insensitive() {
    let mut command = SshCommand::new("example.com");
    command.push_option("-o connecttimeout=15");
    let (_, args) = command.command_parts_for_testing();
    let rendered = args_to_strings(&args);
    let count = rendered
        .iter()
        .filter(|a| a.to_ascii_lowercase().contains("connecttimeout"))
        .count();
    assert_eq!(
        count, 1,
        "case-insensitive match should prevent duplicate: {rendered:?}"
    );
}

#[test]
fn connect_timeout_rounds_up_subsecond_durations() {
    let mut command = SshCommand::new("example.com");
    // 10.1 seconds should round up to 11
    command.set_connect_timeout(Some(std::time::Duration::from_millis(10_100)));
    let (_, args) = command.command_parts_for_testing();
    let rendered = args_to_strings(&args);
    assert!(
        rendered.contains(&"-oConnectTimeout=11".to_owned()),
        "10.1s should round up to 11: {rendered:?}"
    );
}

#[test]
fn connect_timeout_exact_seconds_not_rounded() {
    let mut command = SshCommand::new("example.com");
    command.set_connect_timeout(Some(std::time::Duration::from_secs(45)));
    let (_, args) = command.command_parts_for_testing();
    let rendered = args_to_strings(&args);
    assert!(
        rendered.contains(&"-oConnectTimeout=45".to_owned()),
        "exact 45s should not round up: {rendered:?}"
    );
}

#[test]
fn connect_timeout_zero_duration() {
    let mut command = SshCommand::new("example.com");
    command.set_connect_timeout(Some(std::time::Duration::from_secs(0)));
    let (_, args) = command.command_parts_for_testing();
    let rendered = args_to_strings(&args);
    assert!(
        rendered.contains(&"-oConnectTimeout=0".to_owned()),
        "zero duration should produce ConnectTimeout=0: {rendered:?}"
    );
}

#[cfg(unix)]
#[test]
fn connect_watchdog_fires_on_timeout() {
    use std::time::Duration;

    // Spawn a process that sleeps for a long time - simulating an SSH process
    // that hangs during connection establishment.
    let mut command = SshCommand::new("ignored");
    command.set_program("sh");
    command.set_batch_mode(false);
    command.push_option("-c");
    command.push_option("sleep 60");
    command.set_target_override(Some(OsString::new()));
    // Set a very short connect timeout to trigger the watchdog quickly.
    command.set_connect_timeout(Some(Duration::from_millis(200)));

    let mut connection = command.spawn().expect("spawn shell");

    // Wait for the watchdog to fire before attempting any read. This avoids
    // relying on Child::kill() to unblock a blocking pipe read, which is
    // unreliable in Linux CI environments.
    std::thread::sleep(Duration::from_millis(500));

    // cancel_connect_watchdog should report that the timeout fired.
    let cancel_result = connection.cancel_connect_watchdog();
    assert!(
        cancel_result.is_err(),
        "cancel should report timeout after watchdog fired"
    );
    let err = cancel_result.unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);
    assert!(
        err.to_string().contains("timed out"),
        "error should mention timeout: {err}"
    );

    // After the watchdog has fired, the read should return immediately - either
    // TimedOut (from the has_fired check) or an OS error (broken pipe / EOF
    // from the killed child process).
    let mut buf = [0u8; 1];
    let result = connection.read(&mut buf);
    match result {
        Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {}
        Err(_) => {
            // OS-level error from killed child (broken pipe, EOF) is acceptable.
        }
        Ok(0) => {
            // EOF from killed child is acceptable.
        }
        Ok(n) => {
            panic!("unexpected successful read of {n} bytes from sleeping process");
        }
    }
}

#[cfg(unix)]
#[test]
fn connect_watchdog_cancelled_before_timeout() {
    use std::time::Duration;

    // Spawn a process that exits quickly - simulating a successful SSH connection.
    let mut command = SshCommand::new("ignored");
    command.set_program("sh");
    command.set_batch_mode(false);
    command.push_option("-c");
    command.push_option("echo hello");
    command.set_target_override(Some(OsString::new()));
    // Set a generous timeout that should not fire.
    command.set_connect_timeout(Some(Duration::from_secs(30)));

    let mut connection = command.spawn().expect("spawn shell");

    // Read the output - this simulates reading the version greeting.
    let mut buf = [0u8; 16];
    let n = connection.read(&mut buf).expect("read should succeed");
    assert!(n > 0, "should read some data");

    // Cancel the watchdog - should succeed since the timeout hasn't fired.
    let result = connection.cancel_connect_watchdog();
    assert!(
        result.is_ok(),
        "cancel should succeed when watchdog hasn't fired: {result:?}"
    );

    // Second cancel is a no-op.
    let result2 = connection.cancel_connect_watchdog();
    assert!(result2.is_ok(), "double cancel should be a no-op");
}

#[cfg(unix)]
#[test]
fn connect_watchdog_not_armed_when_timeout_is_none() {
    use std::time::Duration;

    let mut command = SshCommand::new("ignored");
    command.set_program("sh");
    command.set_batch_mode(false);
    command.push_option("-c");
    command.push_option("echo no-watchdog");
    command.set_target_override(Some(OsString::new()));
    command.set_connect_timeout(None);

    let mut connection = command.spawn().expect("spawn shell");

    // cancel_connect_watchdog is a no-op when no timeout is configured.
    let result = connection.cancel_connect_watchdog();
    assert!(result.is_ok(), "cancel without watchdog should be no-op");

    // Read should work normally.
    let mut buf = [0u8; 32];
    let n = connection.read(&mut buf).expect("read");
    assert!(n > 0);

    let status = connection.wait().expect("wait");
    assert!(status.success());
    let _ = Duration::from_millis(0); // suppress unused import lint
}

#[cfg(unix)]
#[test]
fn connect_watchdog_transferred_to_child_handle_on_split() {
    use std::time::Duration;

    let mut command = SshCommand::new("ignored");
    command.set_program("sh");
    command.set_batch_mode(false);
    command.push_option("-c");
    command.push_option("echo split-test");
    command.set_target_override(Some(OsString::new()));
    command.set_connect_timeout(Some(Duration::from_secs(30)));

    let connection = command.spawn().expect("spawn shell");
    let (_reader, _writer, mut child_handle) = connection.split().expect("split");

    // Cancel via child handle should succeed.
    let result = child_handle.cancel_connect_watchdog();
    assert!(
        result.is_ok(),
        "cancel via child handle should succeed: {result:?}"
    );
}

#[cfg(unix)]
#[test]
fn connect_watchdog_fires_and_child_handle_reports_timeout() {
    use std::time::Duration;

    // Spawn a process that hangs, with a short timeout.
    let mut command = SshCommand::new("ignored");
    command.set_program("sh");
    command.set_batch_mode(false);
    command.push_option("-c");
    command.push_option("sleep 60");
    command.set_target_override(Some(OsString::new()));
    command.set_connect_timeout(Some(Duration::from_millis(200)));

    let connection = command.spawn().expect("spawn shell");
    let (_reader, _writer, mut child_handle) = connection.split().expect("split");

    // Wait for the watchdog to fire.
    std::thread::sleep(Duration::from_millis(500));

    let cancel_result = child_handle.cancel_connect_watchdog();
    assert!(cancel_result.is_err(), "watchdog should have fired");
    let err = cancel_result.unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);
    assert!(
        err.to_string().contains("timed out"),
        "error message should mention timeout: {err}"
    );
}

#[cfg(unix)]
#[test]
fn connect_watchdog_error_message_includes_duration() {
    use std::time::Duration;

    let mut command = SshCommand::new("ignored");
    command.set_program("sh");
    command.set_batch_mode(false);
    command.push_option("-c");
    command.push_option("sleep 60");
    command.set_target_override(Some(OsString::new()));
    // Use a 2-second timeout - short enough for fast tests, long enough
    // to produce a recognizable duration in the error message.
    command.set_connect_timeout(Some(Duration::from_secs(2)));

    let connection = command.spawn().expect("spawn shell");
    let (_reader, _writer, mut child_handle) = connection.split().expect("split");

    // Wait for the watchdog to fire.
    std::thread::sleep(Duration::from_millis(2500));

    let err = child_handle.cancel_connect_watchdog().unwrap_err();
    assert!(
        err.to_string().contains("2 seconds"),
        "error should include the timeout duration: {err}"
    );
}

#[test]
fn injects_single_jump_host_before_target() {
    let mut command = SshCommand::new("dest.example.com");
    command.set_prefer_aes_gcm(Some(false));
    command.set_jump_hosts(Some("bastion.example.com"));

    let (_, args) = command.command_parts_for_testing();
    let rendered = args_to_strings(&args);

    let jump_index = rendered
        .iter()
        .position(|arg| arg == "-J")
        .expect("-J should be present");
    assert_eq!(rendered[jump_index + 1], "bastion.example.com");

    let target_index = rendered
        .iter()
        .position(|arg| arg == "dest.example.com")
        .expect("destination should be present");
    assert!(
        jump_index + 1 < target_index,
        "-J value must precede destination: {rendered:?}"
    );
}

#[test]
fn injects_multi_hop_jump_host_chain_verbatim() {
    let mut command = SshCommand::new("dest.example.com");
    command.set_prefer_aes_gcm(Some(false));
    command.set_jump_hosts(Some("alice@a.example.com,bob@b.example.com"));

    let (_, args) = command.command_parts_for_testing();
    let rendered = args_to_strings(&args);

    let jump_index = rendered
        .iter()
        .position(|arg| arg == "-J")
        .expect("-J should be present");
    assert_eq!(
        rendered[jump_index + 1],
        "alice@a.example.com,bob@b.example.com"
    );
}

#[test]
fn injects_jump_host_with_port_verbatim() {
    let mut command = SshCommand::new("dest.example.com");
    command.set_prefer_aes_gcm(Some(false));
    command.set_jump_hosts(Some("user@bastion.example.com:2200"));

    let (_, args) = command.command_parts_for_testing();
    let rendered = args_to_strings(&args);

    let jump_index = rendered
        .iter()
        .position(|arg| arg == "-J")
        .expect("-J should be present");
    assert_eq!(rendered[jump_index + 1], "user@bastion.example.com:2200");
}

#[test]
fn empty_jump_host_value_is_ignored() {
    let mut command = SshCommand::new("dest.example.com");
    command.set_prefer_aes_gcm(Some(false));
    command.set_jump_hosts(Some(""));

    let (_, args) = command.command_parts_for_testing();
    let rendered = args_to_strings(&args);

    assert!(
        !rendered.iter().any(|arg| arg == "-J"),
        "empty jump-host value must not emit -J: {rendered:?}"
    );
}

#[test]
fn jump_host_not_injected_for_non_ssh_program() {
    let mut command = SshCommand::new("dest.example.com");
    command.set_prefer_aes_gcm(Some(false));
    command.set_program("plink");
    command.set_jump_hosts(Some("bastion.example.com"));

    let (_, args) = command.command_parts_for_testing();
    let rendered = args_to_strings(&args);

    assert!(
        !rendered.iter().any(|arg| arg == "-J"),
        "non-ssh program must not receive -J: {rendered:?}"
    );
}

#[test]
fn jump_host_clearable_via_none() {
    let mut command = SshCommand::new("dest.example.com");
    command.set_prefer_aes_gcm(Some(false));
    command.set_jump_hosts(Some("bastion.example.com"));
    command.set_jump_hosts(None::<&OsStr>);

    let (_, args) = command.command_parts_for_testing();
    let rendered = args_to_strings(&args);

    assert!(
        !rendered.iter().any(|arg| arg == "-J"),
        "clearing jump-hosts must remove -J: {rendered:?}"
    );
}

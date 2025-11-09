use super::common::*;
use super::*;

#[test]
fn module_list_username_prefix_is_accepted() {
    let (addr, handle) = spawn_stub_daemon(vec![
        "@RSYNCD: OK\n",
        "module\tWith comment\n",
        "@RSYNCD: EXIT\n",
    ]);

    let url = format!("rsync://user@{}:{}/", addr.ip(), addr.port());
    let (code, stdout, stderr) = run_with_args([OsString::from(RSYNC), OsString::from(url)]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let rendered = String::from_utf8(stdout).expect("output is UTF-8");
    assert!(rendered.contains("module\tWith comment"));

    handle.join().expect("server thread");
}

#[test]
fn module_list_uses_connect_program_option() {
    let _env_lock = ENV_LOCK.lock().expect("env lock");

    let temp = tempfile::tempdir().expect("tempdir");
    let script_path = temp.path().join("connect-program.sh");
    let script = r#"#!/bin/sh
set -eu
printf "@RSYNCD: 31.0\n"
read _greeting
printf "@RSYNCD: OK\n"
read _request
printf "example\tvia connect program\n@RSYNCD: EXIT\n"
"#;
    write_executable_script(&script_path, script);

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from(format!("--connect-program={}", script_path.display())),
        OsString::from("rsync://example/"),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("module listing is UTF-8");
    assert!(rendered.contains("example\tvia connect program"));
}

#[test]
fn module_list_username_prefix_legacy_syntax_is_accepted() {
    let (addr, handle) = spawn_stub_daemon(vec!["@RSYNCD: OK\n", "module\n", "@RSYNCD: EXIT\n"]);

    let url = format!("user@[{}]:{}::", addr.ip(), addr.port());
    let (code, stdout, stderr) = run_with_args([OsString::from(RSYNC), OsString::from(url)]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let rendered = String::from_utf8(stdout).expect("output is UTF-8");
    assert!(rendered.contains("module"));

    handle.join().expect("server thread");
}

#[test]
fn module_list_uses_password_file_for_authentication() {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD_NO_PAD;
    use checksums::strong::Sha512;
    use tempfile::tempdir;

    let challenge = "pw-test";
    let secret = b"cli-secret";
    let expected_digest = {
        let mut hasher = Sha512::new();
        hasher.update(secret);
        hasher.update(challenge.as_bytes());
        let digest = hasher.finalize();
        STANDARD_NO_PAD.encode(digest)
    };

    let expected_credentials = format!("user {expected_digest}");
    let (addr, handle) = spawn_auth_stub_daemon(
        challenge,
        expected_credentials,
        vec!["@RSYNCD: OK\n", "secure\n", "@RSYNCD: EXIT\n"],
    );

    let temp = tempdir().expect("tempdir");
    let password_path = temp.path().join("daemon.pw");
    std::fs::write(&password_path, b"cli-secret\n").expect("write password");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = std::fs::metadata(&password_path)
            .expect("metadata")
            .permissions();
        permissions.set_mode(0o600);
        std::fs::set_permissions(&password_path, permissions).expect("set permissions");
    }

    let url = format!("rsync://user@{}:{}/", addr.ip(), addr.port());
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from(format!("--password-file={}", password_path.display())),
        OsString::from(url),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("module listing is UTF-8");
    assert!(rendered.contains("secure"));

    handle.join().expect("server thread");
}

#[test]
fn module_list_reads_password_from_stdin() {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD_NO_PAD;
    use checksums::strong::Sha512;

    let challenge = "stdin-test";
    let secret = b"stdin-secret";
    let expected_digest = {
        let mut hasher = Sha512::new();
        hasher.update(secret);
        hasher.update(challenge.as_bytes());
        let digest = hasher.finalize();
        STANDARD_NO_PAD.encode(digest)
    };

    let expected_credentials = format!("user {expected_digest}");
    let (addr, handle) = spawn_auth_stub_daemon(
        challenge,
        expected_credentials,
        vec!["@RSYNCD: OK\n", "secure\n", "@RSYNCD: EXIT\n"],
    );

    set_password_stdin_input(b"stdin-secret\n".to_vec());

    let url = format!("rsync://user@{}:{}/", addr.ip(), addr.port());
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--password-file=-"),
        OsString::from(url),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("module listing is UTF-8");
    assert!(rendered.contains("secure"));

    handle.join().expect("server thread");
}

#[cfg(unix)]
#[test]
fn module_list_rejects_world_readable_password_file() {
    use tempfile::tempdir;

    let temp = tempdir().expect("tempdir");
    let password_path = temp.path().join("insecure.pw");
    std::fs::write(&password_path, b"secret\n").expect("write password");
    {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = std::fs::metadata(&password_path)
            .expect("metadata")
            .permissions();
        permissions.set_mode(0o644);
        std::fs::set_permissions(&password_path, permissions).expect("set perms");
    }

    let url = String::from("rsync://user@127.0.0.1:873/");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from(format!("--password-file={}", password_path.display())),
        OsString::from(url),
    ]);

    assert_eq!(code, 1);
    assert!(stdout.is_empty());
    let rendered = String::from_utf8(stderr).expect("diagnostic is UTF-8");
    assert!(rendered.contains("password file"));
    assert!(rendered.contains("0600"));

    // No server was contacted: the permission error occurs before negotiation.
}

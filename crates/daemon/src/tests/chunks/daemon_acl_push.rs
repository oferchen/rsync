/// End-to-end test for ACL preservation during a daemon push transfer with `-A`.
///
/// Verifies that POSIX ACLs set on source files survive the roundtrip through
/// the wire protocol and are correctly applied at the destination. The test
/// creates files with extended ACL entries (user-specific grants) and confirms
/// they appear on the destination after a daemon push.
///
/// # Filesystem Probe
///
/// ACLs are not supported on all filesystems (e.g., tmpfs on some kernels).
/// The test probes ACL support by attempting to set and read back an ACL on a
/// scratch file in the temp directory. If the filesystem does not support ACLs,
/// the test skips gracefully.
///
/// # Platform
///
/// Linux requires `libacl`; macOS uses NFSv4-style extended ACLs. The test is
/// gated on unix + the `acl` feature. On macOS, only named user/group entries
/// are set since base permissions use file mode bits directly.
///
/// # Upstream Reference
///
/// - `acls.c:send_acl()` / `receive_acl()` - wire encoding of ACL entries
/// - `acls.c:set_acl()` - application of received ACLs to destination files
/// - `options.c` - `-A` sets `preserve_acls`
#[cfg(all(unix, feature = "acl"))]
#[test]
fn daemon_acl_push_preserves_acls() {
    use exacl::{AclEntry, AclEntryKind, AclOption, Perm, getfacl, setfacl};
    use std::os::unix::fs::PermissionsExt;

    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let temp = tempdir().expect("tempdir");

    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    let acl_option = AclOption::ACCESS_ACL;
    #[cfg(target_os = "macos")]
    let acl_option = AclOption::empty();

    // --- Probe ACL support on this filesystem ---
    // Some CI runners use tmpfs or other filesystems that lack ACL support.
    // Write a scratch file, set an extended ACL, read it back, and skip if
    // the round-trip fails.
    let probe_file = temp.path().join("acl_probe");
    fs::write(&probe_file, b"probe\n").expect("write probe file");

    // Build a minimal extended ACL with a named user entry (uid 65534).
    let probe_uid_str = "65534";

    // On Linux/FreeBSD, POSIX ACLs require base entries (user_obj, group_obj,
    // mask, other) alongside named entries. On macOS, only named entries are
    // used since base permissions come from file mode bits.
    let probe_entries: Vec<AclEntry> = {
        #[cfg(any(target_os = "linux", target_os = "freebsd"))]
        {
            vec![
                AclEntry::allow_user("", Perm::READ | Perm::WRITE, None),
                AclEntry::allow_group("", Perm::READ, None),
                AclEntry::allow_mask(Perm::READ | Perm::WRITE | Perm::EXECUTE, None),
                AclEntry::allow_other(Perm::empty(), None),
                AclEntry::allow_user(probe_uid_str, Perm::READ, None),
            ]
        }
        #[cfg(target_os = "macos")]
        {
            vec![AclEntry::allow_user(probe_uid_str, Perm::READ, None)]
        }
    };

    if setfacl(&[&probe_file], &probe_entries, acl_option).is_err() {
        eprintln!("skipping daemon_acl_push: filesystem does not support setfacl");
        return;
    }

    let readback = match getfacl(&probe_file, acl_option) {
        Ok(entries) => entries,
        Err(_) => {
            eprintln!("skipping daemon_acl_push: filesystem does not support getfacl");
            return;
        }
    };

    let has_extended_entry = readback.iter().any(|e| {
        e.kind == AclEntryKind::User && !e.name.is_empty() && e.perms.contains(Perm::READ)
    });

    if !has_extended_entry {
        eprintln!(
            "skipping daemon_acl_push: extended ACL entry not preserved on this filesystem"
        );
        return;
    }

    // --- Source tree ---
    let source_dir = temp.path().join("source");
    fs::create_dir(&source_dir).expect("create source");

    let file_a = source_dir.join("alpha.txt");
    let file_b = source_dir.join("beta.txt");
    fs::write(&file_a, b"alpha content\n").expect("write alpha.txt");
    fs::write(&file_b, b"beta content\n").expect("write beta.txt");

    // Set base permissions so the ACL entries are meaningful.
    let mut perms = fs::metadata(&file_a).expect("alpha metadata").permissions();
    perms.set_mode(0o644);
    fs::set_permissions(&file_a, perms.clone()).expect("set alpha perms");
    fs::set_permissions(&file_b, perms).expect("set beta perms");

    // Set extended ACLs on both source files: grant read+execute to uid 65534.
    let source_acl_entries: Vec<AclEntry> = {
        #[cfg(any(target_os = "linux", target_os = "freebsd"))]
        {
            vec![
                AclEntry::allow_user("", Perm::READ | Perm::WRITE, None),
                AclEntry::allow_group("", Perm::READ, None),
                AclEntry::allow_mask(Perm::READ | Perm::WRITE | Perm::EXECUTE, None),
                AclEntry::allow_other(Perm::READ, None),
                AclEntry::allow_user(probe_uid_str, Perm::READ | Perm::EXECUTE, None),
            ]
        }
        #[cfg(target_os = "macos")]
        {
            vec![AclEntry::allow_user(
                probe_uid_str,
                Perm::READ | Perm::EXECUTE,
                None,
            )]
        }
    };

    setfacl(&[&file_a], &source_acl_entries, acl_option).expect("setfacl on alpha.txt");
    setfacl(&[&file_b], &source_acl_entries, acl_option).expect("setfacl on beta.txt");

    // --- Destination (served by daemon, writable, initially empty) ---
    let dest_dir = temp.path().join("dest");
    fs::create_dir(&dest_dir).expect("create dest");

    // --- Daemon config ---
    let config_file = temp.path().join("rsyncd.conf");
    let config_content = format!(
        "[pushmod]\n\
         path = {}\n\
         read only = false\n\
         use chroot = false\n",
        dest_dir.display()
    );
    fs::write(&config_file, config_content).expect("write daemon config");

    let (port, held_listener) = allocate_test_port();

    let daemon_config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--config"),
            config_file.as_os_str().to_owned(),
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--max-sessions"),
            OsString::from("2"),
        ])
        .build();

    let (probe_stream, daemon_handle) = start_daemon(daemon_config, port, held_listener);
    drop(probe_stream);

    // --- Run client push with ACL preservation ---
    let mut source_arg = source_dir.clone().into_os_string();
    source_arg.push("/");
    let rsync_url = format!("rsync://127.0.0.1:{port}/pushmod/");

    let client_config = core::client::ClientConfig::builder()
        .transfer_args([source_arg, OsString::from(&rsync_url)])
        .acls(true)
        .build();

    let result = core::client::run_client(client_config);

    match &result {
        Ok(summary) => {
            assert!(
                summary.files_copied() >= 2,
                "expected at least 2 files transferred, got {}",
                summary.files_copied()
            );
        }
        Err(e) => {
            let _ = daemon_handle.join();
            panic!("ACL push failed: {e}");
        }
    }

    // --- Verify destination file contents ---
    let dest_alpha = dest_dir.join("alpha.txt");
    let dest_beta = dest_dir.join("beta.txt");

    assert!(dest_alpha.exists(), "alpha.txt must exist at destination");
    assert!(dest_beta.exists(), "beta.txt must exist at destination");

    assert_eq!(
        fs::read(&dest_alpha).expect("read dest alpha"),
        b"alpha content\n",
        "alpha.txt content mismatch"
    );
    assert_eq!(
        fs::read(&dest_beta).expect("read dest beta"),
        b"beta content\n",
        "beta.txt content mismatch"
    );

    // --- Verify ACLs are preserved at the destination ---
    let dest_alpha_acl =
        getfacl(&dest_alpha, acl_option).expect("getfacl on destination alpha.txt");
    let dest_beta_acl =
        getfacl(&dest_beta, acl_option).expect("getfacl on destination beta.txt");

    // Check that the extended user ACL entry (uid 65534 with read+execute) is present.
    let check_acl = |entries: &[AclEntry], file_name: &str| {
        let extended_user = entries
            .iter()
            .find(|e| e.kind == AclEntryKind::User && !e.name.is_empty());

        let entry = extended_user.unwrap_or_else(|| {
            panic!("{file_name}: no extended user ACL entry found; entries: {entries:?}");
        });

        assert!(
            entry.perms.contains(Perm::READ),
            "{file_name}: extended user ACL must have READ permission, got {:?}",
            entry.perms
        );
        assert!(
            entry.perms.contains(Perm::EXECUTE),
            "{file_name}: extended user ACL must have EXECUTE permission, got {:?}",
            entry.perms
        );
    };

    check_acl(&dest_alpha_acl, "alpha.txt");
    check_acl(&dest_beta_acl, "beta.txt");

    let daemon_result = daemon_handle.join().expect("daemon thread");
    let _ = daemon_result;
}

/// End-to-end test for combined `--hard-links --relative` (`-H -R`) push over
/// daemon protocol.
///
/// Verifies that a push transfer with both `-H` and `-R` correctly preserves
/// hard links at the destination, even when `--relative` changes the file list
/// sort order so that wire-order `XMIT_HLINK_FIRST` flags disagree with
/// sorted-order leaders. The fix in `match_hard_links()` reassigns leader and
/// follower roles after sorting, mirroring upstream `hlink.c:match_hard_links()`.
///
/// # Scenario
///
/// Source (client side):
///   x/alpha.txt         (regular file, "shared content")
///   x/alpha_link.txt    (hard link to x/alpha.txt)
///   a/b/beta.txt        (regular file, "beta content")
///   a/b/beta_link.txt   (hard link to a/b/beta.txt)
///
/// The nested `a/b/` prefix causes those entries to sort before `x/` in the
/// file list, while the wire order sends `x/` first. This exercises the
/// leader/follower reassignment after sorting.
///
/// Destination (daemon module, initially empty):
///   After push with `-H -R`, the destination should contain the full nested
///   directory structure with hard-link relationships preserved:
///     x/alpha.txt and x/alpha_link.txt share an inode
///     a/b/beta.txt and a/b/beta_link.txt share an inode
///
/// # Upstream Reference
///
/// - `hlink.c:match_hard_links()` - reassigns leader after file list sorting
/// - `flist.c` - `XMIT_HLINK_FIRST` flag in wire encoding
/// - `options.c` - `-H` sets `preserve_hard_links`, `-R` sets `relative_paths`
#[cfg(unix)]
#[test]
fn daemon_hardlinks_relative_receive_preserves_links() {
    use std::os::unix::fs::MetadataExt;

    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let temp = tempdir().expect("tempdir");

    // --- Source tree (client side) with nested directories ---
    let source_dir = temp.path().join("source");

    // Create x/ directory with a hard-link pair
    let x_dir = source_dir.join("x");
    fs::create_dir_all(&x_dir).expect("create source/x");
    fs::write(x_dir.join("alpha.txt"), b"shared content\n").expect("write alpha.txt");
    fs::hard_link(x_dir.join("alpha.txt"), x_dir.join("alpha_link.txt"))
        .expect("hard link alpha_link.txt");

    // Create a/b/ directory with another hard-link pair.
    // The a/b/ prefix sorts before x/ so the sorted file list order differs
    // from wire order - this is the key scenario that exercises leader
    // reassignment.
    let ab_dir = source_dir.join("a").join("b");
    fs::create_dir_all(&ab_dir).expect("create source/a/b");
    fs::write(ab_dir.join("beta.txt"), b"beta content\n").expect("write beta.txt");
    fs::hard_link(ab_dir.join("beta.txt"), ab_dir.join("beta_link.txt"))
        .expect("hard link beta_link.txt");

    // Verify source hard links are correct before transfer
    let alpha_ino = fs::metadata(x_dir.join("alpha.txt"))
        .expect("alpha metadata")
        .ino();
    let alpha_link_ino = fs::metadata(x_dir.join("alpha_link.txt"))
        .expect("alpha_link metadata")
        .ino();
    assert_eq!(
        alpha_ino, alpha_link_ino,
        "source alpha.txt and alpha_link.txt must share an inode"
    );

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

    // --- Run client push with -H -R ---
    let mut source_arg = source_dir.clone().into_os_string();
    source_arg.push("/");
    let rsync_url = format!("rsync://127.0.0.1:{port}/pushmod/");

    let client_config = core::client::ClientConfig::builder()
        .transfer_args([source_arg, OsString::from(&rsync_url)])
        .hard_links(true)
        .relative_paths(true)
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
            panic!("hardlinks-relative client push failed: {e}");
        }
    }

    // Verify full nested directory structure was reconstructed
    let dest_alpha = dest_dir.join("x").join("alpha.txt");
    let dest_alpha_link = dest_dir.join("x").join("alpha_link.txt");
    let dest_beta = dest_dir.join("a").join("b").join("beta.txt");
    let dest_beta_link = dest_dir.join("a").join("b").join("beta_link.txt");

    assert!(dest_alpha.exists(), "x/alpha.txt must exist at destination");
    assert!(
        dest_alpha_link.exists(),
        "x/alpha_link.txt must exist at destination"
    );
    assert!(
        dest_beta.exists(),
        "a/b/beta.txt must exist at destination"
    );
    assert!(
        dest_beta_link.exists(),
        "a/b/beta_link.txt must exist at destination"
    );

    // Verify file contents
    assert_eq!(
        fs::read(&dest_alpha).expect("read alpha.txt"),
        b"shared content\n",
        "alpha.txt content mismatch"
    );
    assert_eq!(
        fs::read(&dest_beta).expect("read beta.txt"),
        b"beta content\n",
        "beta.txt content mismatch"
    );

    // Verify hard-link preservation: alpha pair must share an inode
    let dest_alpha_meta = fs::metadata(&dest_alpha).expect("dest alpha metadata");
    let dest_alpha_link_meta = fs::metadata(&dest_alpha_link).expect("dest alpha_link metadata");

    assert_eq!(
        dest_alpha_meta.ino(),
        dest_alpha_link_meta.ino(),
        "x/alpha.txt and x/alpha_link.txt must share an inode (hard link preserved)"
    );
    assert!(
        dest_alpha_meta.nlink() >= 2,
        "x/alpha.txt nlink must be >= 2 (hard link), got {}",
        dest_alpha_meta.nlink()
    );

    // Verify hard-link preservation: beta pair must share an inode
    let dest_beta_meta = fs::metadata(&dest_beta).expect("dest beta metadata");
    let dest_beta_link_meta = fs::metadata(&dest_beta_link).expect("dest beta_link metadata");

    assert_eq!(
        dest_beta_meta.ino(),
        dest_beta_link_meta.ino(),
        "a/b/beta.txt and a/b/beta_link.txt must share an inode (hard link preserved)"
    );
    assert!(
        dest_beta_meta.nlink() >= 2,
        "a/b/beta.txt nlink must be >= 2 (hard link), got {}",
        dest_beta_meta.nlink()
    );

    // Verify implied parent directories
    assert!(
        dest_dir.join("x").is_dir(),
        "implied directory 'x' must exist"
    );
    assert!(
        dest_dir.join("a").is_dir(),
        "implied directory 'a' must exist"
    );
    assert!(
        dest_dir.join("a").join("b").is_dir(),
        "implied directory 'a/b' must exist"
    );

    // Daemon exits after serving max_sessions connections
    let daemon_result = daemon_handle.join().expect("daemon thread");
    let _ = daemon_result;
}

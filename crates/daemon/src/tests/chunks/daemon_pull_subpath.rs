/// End-to-end tests for daemon module sub-path pull resolution.
///
/// Mirrors upstream `clientserver.c:1073 read_args()` + `util1.c:804
/// glob_expand_module()` + `flist.c:2338-2349` sender per-positional split:
/// when a client requests `rsync://h/mod/d1/d2/f2`, the wire emits a single
/// file-list entry whose name is the basename (`f2`), so the receiver writes
/// it directly under the destination directory instead of recreating the
/// full sub-path.
///
/// Each test stands up a temporary daemon module, issues a pull, and asserts
/// the destination layout. The cases cover the four upstream-supported sub-
/// path shapes: a single file, a sub-directory with trailing slash, a sub-
/// directory without trailing slash (oc-rsync's non-relative-mode dotdir
/// semantics flatten the leaf), and a deeply nested file.
///
/// # Upstream Reference
///
/// - `clientserver.c:1073` - `read_args()` with `mod_name` triggers `glob_expand_module`
/// - `util1.c:804`         - `glob_expand_module()` strips the module prefix
/// - `flist.c:2338-2349`   - per-positional `dir/fn` split before `link_stat`

#[cfg(unix)]
fn write_subpath_daemon_config(temp: &Path, module_dir: &Path) -> PathBuf {
    let config_file = temp.join("rsyncd.conf");
    let config_content = format!(
        "[sub]\n\
         path = {}\n\
         read only = true\n\
         use chroot = false\n",
        module_dir.display()
    );
    fs::write(&config_file, config_content).expect("write daemon config");
    config_file
}

#[cfg(unix)]
fn launch_subpath_daemon(
    config_file: &Path,
    port: u16,
    held: std::net::TcpListener,
) -> (
    std::net::TcpStream,
    std::thread::JoinHandle<Result<(), DaemonError>>,
) {
    let daemon_config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--config"),
            config_file.as_os_str().to_owned(),
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--max-sessions"),
            OsString::from("3"),
        ])
        .build();
    start_daemon(daemon_config, port, held)
}

#[cfg(unix)]
fn build_subpath_module_tree(module_dir: &Path) {
    let d2 = module_dir.join("d1").join("d2");
    fs::create_dir_all(&d2).expect("create d1/d2");
    fs::write(d2.join("f2"), b"sub-path leaf content\n").expect("write f2");
    fs::write(d2.join("sibling.txt"), b"sibling under d2\n").expect("write sibling");
    // Add an unrelated file outside the requested sub-path so we can prove the
    // sender does NOT walk the whole module root for a sub-path request.
    fs::write(module_dir.join("unrelated.txt"), b"should not be transferred\n")
        .expect("write unrelated");
}

#[cfg(unix)]
#[test]
fn daemon_pull_subpath_single_file_emits_basename() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let temp = tempdir().expect("tempdir");
    let module_dir = temp.path().join("module");
    fs::create_dir(&module_dir).expect("create module");
    build_subpath_module_tree(&module_dir);

    let dest = temp.path().join("pulled");
    fs::create_dir(&dest).expect("create dest");

    let (port, held) = allocate_test_port();
    let config_file = write_subpath_daemon_config(temp.path(), &module_dir);
    let (probe, handle) = launch_subpath_daemon(&config_file, port, held);
    drop(probe);

    // upstream: clientserver.c:1073 - the wire arg `sub/d1/d2/f2` is the
    // module name plus the sub-path; glob_expand_module strips `sub` before
    // the sender walks the single positional.
    let url = format!("rsync://127.0.0.1:{port}/sub/d1/d2/f2");
    let client_config = core::client::ClientConfig::builder()
        .transfer_args([OsString::from(&url), OsString::from(dest.as_os_str())])
        .build();
    let result = core::client::run_client(client_config);
    if let Err(e) = &result {
        let _ = handle.join();
        panic!("sub-path pull failed: {e}");
    }

    // upstream: flist.c:2338-2349 - dir=d1/d2, fn=f2; receiver writes <dest>/f2.
    assert_eq!(
        fs::read(dest.join("f2")).expect("read pulled f2"),
        b"sub-path leaf content\n",
        "single-file sub-path pull must land at <dest>/<basename>",
    );
    // The sibling file under the same parent must NOT come along - the sender
    // walked only the explicit positional.
    assert!(
        !dest.join("sibling.txt").exists(),
        "single-file sub-path pull must not enumerate siblings",
    );
    // The unrelated file at module root must NOT come along either.
    assert!(
        !dest.join("unrelated.txt").exists(),
        "single-file sub-path pull must not walk the whole module root",
    );

    let _ = handle.join();
}

#[cfg(unix)]
#[test]
fn daemon_pull_subpath_directory_trailing_slash_flattens_contents() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let temp = tempdir().expect("tempdir");
    let module_dir = temp.path().join("module");
    fs::create_dir(&module_dir).expect("create module");
    build_subpath_module_tree(&module_dir);

    let dest = temp.path().join("pulled");
    fs::create_dir(&dest).expect("create dest");

    let (port, held) = allocate_test_port();
    let config_file = write_subpath_daemon_config(temp.path(), &module_dir);
    let (probe, handle) = launch_subpath_daemon(&config_file, port, held);
    drop(probe);

    // upstream: flist.c:2312-2322 - trailing slash promotes the source to
    // DOTDIR_NAME, walking the directory's contents as `.`/children.
    let url = format!("rsync://127.0.0.1:{port}/sub/d1/d2/");
    let client_config = core::client::ClientConfig::builder()
        .transfer_args([OsString::from(&url), OsString::from(dest.as_os_str())])
        .recursive(true)
        .build();
    let result = core::client::run_client(client_config);
    if let Err(e) = &result {
        let _ = handle.join();
        panic!("trailing-slash sub-path pull failed: {e}");
    }

    assert_eq!(
        fs::read(dest.join("f2")).expect("read pulled f2"),
        b"sub-path leaf content\n",
        "trailing-slash sub-path pull must flatten the sub-directory contents",
    );
    assert_eq!(
        fs::read(dest.join("sibling.txt")).expect("read pulled sibling"),
        b"sibling under d2\n",
        "trailing-slash sub-path pull must include every child of the leaf dir",
    );
    // The unrelated file at module root must NOT come along.
    assert!(
        !dest.join("unrelated.txt").exists(),
        "trailing-slash sub-path pull must not walk above the requested directory",
    );

    let _ = handle.join();
}

#[cfg(unix)]
#[test]
fn daemon_pull_subpath_directory_no_trailing_slash_walks_subtree() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let temp = tempdir().expect("tempdir");
    let module_dir = temp.path().join("module");
    fs::create_dir(&module_dir).expect("create module");
    build_subpath_module_tree(&module_dir);

    let dest = temp.path().join("pulled");
    fs::create_dir(&dest).expect("create dest");

    let (port, held) = allocate_test_port();
    let config_file = write_subpath_daemon_config(temp.path(), &module_dir);
    let (probe, handle) = launch_subpath_daemon(&config_file, port, held);
    drop(probe);

    // upstream: flist.c:2338-2349 - without a trailing slash the source is
    // still a directory: oc-rsync's non-relative walk emits dot + children
    // (matching the `non_relative_mode_uses_basename` regression test),
    // so the destination receives the children directly without a `d2/`
    // wrapper. The critical regression check is that the unrelated module
    // root file is NOT transferred - the sub-path constraint must hold even
    // when the sub-path resolves to a directory.
    let url = format!("rsync://127.0.0.1:{port}/sub/d1/d2");
    let client_config = core::client::ClientConfig::builder()
        .transfer_args([OsString::from(&url), OsString::from(dest.as_os_str())])
        .recursive(true)
        .build();
    let result = core::client::run_client(client_config);
    if let Err(e) = &result {
        let _ = handle.join();
        panic!("no-trailing-slash sub-path pull failed: {e}");
    }

    // Children of d1/d2 must be present (flattened under dest by oc-rsync's
    // non-relative dotdir semantics).
    assert_eq!(
        fs::read(dest.join("f2")).expect("read pulled f2"),
        b"sub-path leaf content\n",
        "directory sub-path pull must transfer leaf contents",
    );
    assert_eq!(
        fs::read(dest.join("sibling.txt")).expect("read pulled sibling"),
        b"sibling under d2\n",
        "directory sub-path pull must transfer all children of the leaf dir",
    );
    // Files outside the requested sub-path must NOT come along - this is the
    // core invariant the fix establishes.
    assert!(
        !dest.join("unrelated.txt").exists(),
        "directory sub-path pull must not escape the requested sub-tree",
    );

    let _ = handle.join();
}

#[cfg(unix)]
#[test]
fn daemon_pull_subpath_deeply_nested_file_emits_basename() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let temp = tempdir().expect("tempdir");
    let module_dir = temp.path().join("module");
    let deep = module_dir.join("a").join("b").join("c").join("d").join("e");
    fs::create_dir_all(&deep).expect("create deep tree");
    fs::write(deep.join("leaf.bin"), b"\x00\x01\x02deep payload").expect("write leaf");

    let dest = temp.path().join("pulled");
    fs::create_dir(&dest).expect("create dest");

    let (port, held) = allocate_test_port();
    let config_file = write_subpath_daemon_config(temp.path(), &module_dir);
    let (probe, handle) = launch_subpath_daemon(&config_file, port, held);
    drop(probe);

    // upstream: flist.c:2338-2349 - the per-positional dir/fn split walks the
    // last `/` regardless of nesting depth, so the wire entry is just `leaf.bin`.
    let url = format!("rsync://127.0.0.1:{port}/sub/a/b/c/d/e/leaf.bin");
    let client_config = core::client::ClientConfig::builder()
        .transfer_args([OsString::from(&url), OsString::from(dest.as_os_str())])
        .build();
    let result = core::client::run_client(client_config);
    if let Err(e) = &result {
        let _ = handle.join();
        panic!("deep sub-path pull failed: {e}");
    }

    assert_eq!(
        fs::read(dest.join("leaf.bin")).expect("read deep leaf"),
        b"\x00\x01\x02deep payload",
        "deeply nested single-file sub-path pull must still land at <dest>/<basename>",
    );
    // No intermediate directory leakage.
    assert!(
        !dest.join("a").exists(),
        "deep sub-path pull must NOT recreate the intermediate path components",
    );

    let _ = handle.join();
}

#[cfg(unix)]
#[test]
fn daemon_pull_subpath_rejects_parent_dir_traversal() {
    // SEC-1.q: a crafted `rsync://h/mod/../etc/passwd` URL must be refused at
    // the daemon's argument-resolution stage. The chroot / Landlock layer
    // covers the same ground inside the sandbox, but defense-in-depth here
    // ensures non-chroot daemons cannot leak files outside the module root.
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let temp = tempdir().expect("tempdir");
    let module_dir = temp.path().join("module");
    fs::create_dir(&module_dir).expect("create module");
    fs::write(module_dir.join("inside.txt"), b"in-module\n").expect("write inside");
    // A "secret" file alongside the module root that the client must not
    // be able to reach via `..` traversal.
    fs::write(temp.path().join("secret.txt"), b"top secret\n").expect("write secret");

    let dest = temp.path().join("pulled");
    fs::create_dir(&dest).expect("create dest");

    let (port, held) = allocate_test_port();
    let config_file = write_subpath_daemon_config(temp.path(), &module_dir);
    let (probe, handle) = launch_subpath_daemon(&config_file, port, held);
    drop(probe);

    let url = format!("rsync://127.0.0.1:{port}/sub/../secret.txt");
    let client_config = core::client::ClientConfig::builder()
        .transfer_args([OsString::from(&url), OsString::from(dest.as_os_str())])
        .build();
    let result = core::client::run_client(client_config);

    assert!(
        result.is_err(),
        "traversal pull must fail; secret file was {}",
        if dest.join("secret.txt").exists() {
            "READ THROUGH"
        } else {
            "not transferred but client did not error"
        },
    );
    assert!(
        !dest.join("secret.txt").exists(),
        "traversal pull must NEVER place a file outside the module on disk",
    );

    let _ = handle.join();
}

//! Receiver transfer-setup regression tests.

#[cfg(all(test, unix))]
mod daemon_incoming_chmod_tests {
    //! Daemon `incoming chmod = SPEC` regression: the receiver must apply the
    //! daemon module's chmod modifiers on top of the destination mode before
    //! the on-disk permissions are finalized. Mirrors upstream
    //! `clientserver.c:rsync_module()` arming `daemon_chmod_modes` and
    //! `receiver.c` invoking it via `set_file_attrs()` at finalize time.

    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    use ::metadata::{ChmodModifiers, MetadataOptions, apply_file_metadata_with_options};

    /// Builds the same `MetadataOptions` chain the receiver-side
    /// `setup_transfer` produces (perms + chmod modifiers + fake_super) for a
    /// daemon-config-driven `incoming chmod`. This is the receiver's runtime
    /// contract: the chmod modifiers must be installed via
    /// `MetadataOptions::with_chmod` so the existing apply path rewrites the
    /// destination mode at finalize time.
    fn receiver_metadata_opts(chmod: Option<ChmodModifiers>) -> MetadataOptions {
        MetadataOptions::new()
            .preserve_permissions(true)
            .preserve_times(false)
            .with_chmod(chmod)
    }

    /// `incoming chmod = F600` forces every received regular file to land on
    /// disk with mode 0o600 regardless of the source's mode bits, matching
    /// upstream's `daemon_chmod_modes` semantics for push transfers.
    #[test]
    fn incoming_chmod_f600_rewrites_dest_to_0600() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let source = tmp.path().join("source.bin");
        let dest = tmp.path().join("dest.bin");
        fs::write(&source, b"payload").expect("write source");
        fs::write(&dest, b"payload").expect("write dest");
        // Source-side mode the sender would have advertised on the wire.
        fs::set_permissions(&source, fs::Permissions::from_mode(0o644)).expect("set source perms");
        // Existing dest mode the receiver overwrites.
        fs::set_permissions(&dest, fs::Permissions::from_mode(0o600)).expect("set dest perms");

        let source_meta = fs::metadata(&source).expect("source metadata");
        let modifiers = ChmodModifiers::parse("F600").expect("parse chmod spec");
        let opts = receiver_metadata_opts(Some(modifiers));

        apply_file_metadata_with_options(&dest, &source_meta, &opts)
            .expect("apply daemon incoming chmod");

        let mode = fs::metadata(&dest)
            .expect("dest metadata")
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o7777,
            0o600,
            "incoming chmod = F600 must force destination mode to 0o600",
        );
    }

    /// Without an `incoming chmod` directive the receiver preserves the
    /// source mode exactly - no silent rewrite, no default fall-through.
    #[test]
    fn no_incoming_chmod_preserves_source_mode() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let source = tmp.path().join("source.bin");
        let dest = tmp.path().join("dest.bin");
        fs::write(&source, b"payload").expect("write source");
        fs::write(&dest, b"payload").expect("write dest");
        fs::set_permissions(&source, fs::Permissions::from_mode(0o640)).expect("set source perms");
        fs::set_permissions(&dest, fs::Permissions::from_mode(0o600)).expect("set dest perms");

        let source_meta = fs::metadata(&source).expect("source metadata");
        let opts = receiver_metadata_opts(None);

        apply_file_metadata_with_options(&dest, &source_meta, &opts)
            .expect("apply metadata without chmod");

        let mode = fs::metadata(&dest)
            .expect("dest metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o7777, 0o640);
    }

    /// `incoming chmod = Fg-r,Fo-r` strips group-read and other-read from
    /// every received regular file. Matches the daemon-filter scenario:
    /// uploading `a.secret` at mode 0o644 must land at 0o600 because the
    /// daemon module strips both group-read and other-read.
    // upstream: testsuite/daemon-filter+chmod parity - mode 0644 -> 0600
    #[test]
    fn incoming_chmod_fg_r_fo_r_strips_world_and_group_read() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let source = tmp.path().join("a.secret");
        let dest = tmp.path().join("a.secret.dest");
        fs::write(&source, b"secret payload").expect("write source");
        fs::write(&dest, b"secret payload").expect("write dest");
        fs::set_permissions(&source, fs::Permissions::from_mode(0o644)).expect("set source perms");
        fs::set_permissions(&dest, fs::Permissions::from_mode(0o644)).expect("set dest perms");

        let source_meta = fs::metadata(&source).expect("source metadata");
        let modifiers = ChmodModifiers::parse("Fg-r,Fo-r").expect("parse chmod spec");
        let opts = receiver_metadata_opts(Some(modifiers));

        apply_file_metadata_with_options(&dest, &source_meta, &opts)
            .expect("apply daemon incoming chmod");

        let mode = fs::metadata(&dest)
            .expect("dest metadata")
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o7777,
            0o600,
            "incoming chmod = Fg-r,Fo-r on 0644 must yield 0600",
        );
    }

    /// `incoming chmod = Fu+rw,go-rwx` is the chmod-option daemon scenario:
    /// uploading `name1` at mode 0o644 must land at 0o600 (user keeps rw,
    /// group and other lose every bit). The second clause has no `F`
    /// prefix; with `target = All` it still applies to files (and would
    /// also apply to dirs, but the destination here is a regular file).
    // upstream: testsuite/chmod-option parity - mode 0644 + go-rwx -> 0600
    #[test]
    fn incoming_chmod_fu_plus_rw_go_minus_rwx_resolves_to_0600() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let source = tmp.path().join("name1");
        let dest = tmp.path().join("name1.dest");
        fs::write(&source, b"This is the file").expect("write source");
        fs::write(&dest, b"This is the file").expect("write dest");
        fs::set_permissions(&source, fs::Permissions::from_mode(0o644)).expect("set source perms");
        fs::set_permissions(&dest, fs::Permissions::from_mode(0o644)).expect("set dest perms");

        let source_meta = fs::metadata(&source).expect("source metadata");
        let modifiers = ChmodModifiers::parse("Fu+rw,go-rwx").expect("parse chmod spec");
        let opts = receiver_metadata_opts(Some(modifiers));

        apply_file_metadata_with_options(&dest, &source_meta, &opts)
            .expect("apply daemon incoming chmod");

        let mode = fs::metadata(&dest)
            .expect("dest metadata")
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o7777,
            0o600,
            "incoming chmod = Fu+rw,go-rwx on 0644 must yield 0600",
        );
    }

    /// Single-source-of-truth: the parsed `ChmodModifiers` and the resulting
    /// destination mode are byte-identical whether the spec arrives via CLI
    /// `--chmod=SPEC` or via the daemon `incoming chmod = SPEC` directive.
    /// Both code paths funnel through `ChmodModifiers::parse`; this test
    /// guards against a future fork (e.g. a duplicate daemon-only parser)
    /// silently diverging.
    // upstream: parity with options.c:parse_chmod() (CLI) and
    // clientserver.c:rsync_module() (daemon)
    #[test]
    fn cli_chmod_and_daemon_incoming_chmod_resolve_identically() {
        let spec = "Fg-r,Fo-r";
        let cli_modifiers = ChmodModifiers::parse(spec).expect("parse via CLI path");
        let daemon_modifiers = ChmodModifiers::parse(spec).expect("parse via daemon path");
        assert_eq!(
            cli_modifiers, daemon_modifiers,
            "CLI and daemon parsers must agree byte-for-byte on the parsed clauses",
        );

        let tmp = tempfile::tempdir().expect("tempdir");
        let source = tmp.path().join("name1");
        let dest_cli = tmp.path().join("name1.cli");
        let dest_daemon = tmp.path().join("name1.daemon");
        fs::write(&source, b"payload").expect("write source");
        fs::write(&dest_cli, b"payload").expect("write dest cli");
        fs::write(&dest_daemon, b"payload").expect("write dest daemon");
        fs::set_permissions(&source, fs::Permissions::from_mode(0o644)).expect("set source perms");
        fs::set_permissions(&dest_cli, fs::Permissions::from_mode(0o644))
            .expect("set dest cli perms");
        fs::set_permissions(&dest_daemon, fs::Permissions::from_mode(0o644))
            .expect("set dest daemon perms");

        let source_meta = fs::metadata(&source).expect("source metadata");
        let cli_opts = receiver_metadata_opts(Some(cli_modifiers));
        let daemon_opts = receiver_metadata_opts(Some(daemon_modifiers));

        apply_file_metadata_with_options(&dest_cli, &source_meta, &cli_opts)
            .expect("apply CLI chmod");
        apply_file_metadata_with_options(&dest_daemon, &source_meta, &daemon_opts)
            .expect("apply daemon incoming chmod");

        let cli_mode = fs::metadata(&dest_cli)
            .expect("dest cli metadata")
            .permissions()
            .mode();
        let daemon_mode = fs::metadata(&dest_daemon)
            .expect("dest daemon metadata")
            .permissions()
            .mode();
        assert_eq!(
            cli_mode & 0o7777,
            daemon_mode & 0o7777,
            "CLI --chmod and daemon incoming chmod must yield identical destination modes",
        );
        assert_eq!(cli_mode & 0o7777, 0o600);
    }

    /// Caps-X (`X`, conditional-exec) parity: `Da+X,Fa+X` adds execute
    /// only when the entry is already executable or is a directory. A
    /// regular file with mode 0o644 (no execute bits) must keep its
    /// exec bits cleared; a directory at 0o700 must gain `a+x` and land
    /// at 0o711.
    // upstream: chmod.c:tweak_mode FLAG_X_KEEP - "!IsX && !S_ISDIR" branch
    #[test]
    fn caps_x_conditional_exec_only_applies_when_already_executable_or_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");

        // Regular file path: no exec bits on source -> no exec bits added.
        let source_file = tmp.path().join("plain.txt");
        let dest_file = tmp.path().join("plain.txt.dest");
        fs::write(&source_file, b"plain").expect("write source file");
        fs::write(&dest_file, b"plain").expect("write dest file");
        fs::set_permissions(&source_file, fs::Permissions::from_mode(0o644))
            .expect("set source file perms");
        fs::set_permissions(&dest_file, fs::Permissions::from_mode(0o644))
            .expect("set dest file perms");

        // Directory path: D-clause must apply `a+X` unconditionally on dirs.
        let source_dir = tmp.path().join("source_dir");
        let dest_dir = tmp.path().join("dest_dir");
        fs::create_dir(&source_dir).expect("mkdir source");
        fs::create_dir(&dest_dir).expect("mkdir dest");
        fs::set_permissions(&source_dir, fs::Permissions::from_mode(0o700))
            .expect("set source dir perms");
        fs::set_permissions(&dest_dir, fs::Permissions::from_mode(0o700))
            .expect("set dest dir perms");

        let modifiers = ChmodModifiers::parse("Da+X,Fa+X").expect("parse caps-X spec");

        // File branch.
        let source_file_meta = fs::metadata(&source_file).expect("source file metadata");
        let opts = receiver_metadata_opts(Some(modifiers.clone()));
        apply_file_metadata_with_options(&dest_file, &source_file_meta, &opts)
            .expect("apply caps-X to file");
        let file_mode = fs::metadata(&dest_file)
            .expect("dest file metadata")
            .permissions()
            .mode();
        assert_eq!(
            file_mode & 0o111,
            0,
            "Fa+X must not add exec bits to a non-executable file (mode was {:o})",
            file_mode & 0o7777,
        );

        // Directory branch. The receiver applies dir metadata through the
        // same options pipeline as files (the dir-only D-clause guards the
        // file-only F-clause and vice versa).
        let source_dir_meta = fs::metadata(&source_dir).expect("source dir metadata");
        let dir_opts = receiver_metadata_opts(Some(modifiers));
        apply_file_metadata_with_options(&dest_dir, &source_dir_meta, &dir_opts)
            .expect("apply caps-X to dir");
        let dir_mode = fs::metadata(&dest_dir)
            .expect("dest dir metadata")
            .permissions()
            .mode();
        assert_eq!(
            dir_mode & 0o111,
            0o111,
            "Da+X must add a+x to a directory (mode was {:o})",
            dir_mode & 0o7777,
        );
    }

    /// Builds `MetadataOptions` matching the daemon-upload receiver path
    /// when the client invoked `rsync -avv --no-perms`: permissions are NOT
    /// preserved but the module's `incoming chmod` directive must still
    /// rewrite the destination mode at finalize time. This mirrors upstream
    /// rsync where `--no-perms` only suppresses source-mode propagation
    /// and never overrides the daemon's `daemon_chmod_modes` arming.
    fn receiver_metadata_opts_no_perms(chmod: Option<ChmodModifiers>) -> MetadataOptions {
        MetadataOptions::new()
            .preserve_permissions(false)
            .preserve_times(false)
            .with_chmod(chmod)
    }

    /// UTS-17.REOPEN: daemon-upload with client `--no-perms` must still
    /// apply the daemon module's `incoming chmod` directive. Upstream
    /// rsync's `daemon_chmod_modes` arms at module-load and fires from
    /// the receiver's `set_file_attrs()` regardless of whether `-p` was
    /// negotiated. Regression-guards the chmod-option upstream-testsuite
    /// scenario where the spec `ug-s,a+rX,D+w` must coerce uploaded
    /// files to land with world+group read bits set.
    // upstream: clientserver.c:rsync_module() arms daemon_chmod_modes,
    //           receiver.c:set_file_attrs() applies it independently of -p.
    #[test]
    fn no_perms_does_not_suppress_daemon_incoming_chmod() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let source = tmp.path().join("bar");
        let dest = tmp.path().join("bar.dest");
        fs::write(&source, b"payload").expect("write source");
        fs::write(&dest, b"payload").expect("write dest");
        // upstream: rsync.c:466-470 dest_mode() - for a fresh transfer under
        // `--no-perms` the chmod baseline is `source_mode & (~CHMOD_BITS |
        // dflt_perms)`, where `dflt_perms = ACCESSPERMS & ~orig_umask`. Pin
        // the source mode at 0o600 so the baseline collapses to 0o600 under
        // any umask the test host happens to use (0o022 producing
        // `dflt_perms = 0o755`, 0o002 producing `dflt_perms = 0o775` - both
        // mask 0o600 to 0o600). Using a wider source mode like 0o664 would
        // leave the test environment-fragile: `0o664 & 0o775 = 0o664`
        // already has group-write, so `a+rX` adds nothing and the assertion
        // would fail on container hosts with a relaxed umask.
        fs::set_permissions(&source, fs::Permissions::from_mode(0o600)).expect("set source perms");
        // Existing destination mode is irrelevant for this fresh-transfer
        // path - upstream's `dest_mode(exists=false)` ignores the stat mode -
        // but pin it anyway so the test reads symmetrically.
        fs::set_permissions(&dest, fs::Permissions::from_mode(0o600)).expect("set dest perms");

        let source_meta = fs::metadata(&source).expect("source metadata");
        // `a+rX` is the chmod-option upstream-testsuite incoming-chmod
        // clause that must add read for all (and conditional X, which is
        // a no-op on a non-executable file).
        let modifiers = ChmodModifiers::parse("ug-s,a+rX,D+w").expect("parse chmod spec");
        let opts = receiver_metadata_opts_no_perms(Some(modifiers));

        apply_file_metadata_with_options(&dest, &source_meta, &opts)
            .expect("apply daemon incoming chmod under --no-perms");

        let mode = fs::metadata(&dest)
            .expect("dest metadata")
            .permissions()
            .mode();
        // Starting from 0o600 the spec resolves to 0o644 (add read for all,
        // no exec bits to retain on a non-executable file).
        assert_eq!(
            mode & 0o7777,
            0o644,
            "incoming chmod = ug-s,a+rX,D+w applied to 0o600 destination under --no-perms \
             must lift to 0o644 (mode was {:o})",
            mode & 0o7777,
        );
    }

    /// Hardens the contract: a daemon module with `incoming chmod = F600`
    /// must clamp every uploaded regular file to 0o600 even when the
    /// client passed `--no-perms`. This guards against a future
    /// regression where `--no-perms` short-circuits the chmod-apply
    /// branch.
    // upstream: parity with `incoming chmod = F600` daemon-config tests.
    #[test]
    fn no_perms_with_incoming_chmod_f600_clamps_to_0600() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let source = tmp.path().join("secret");
        let dest = tmp.path().join("secret.dest");
        fs::write(&source, b"payload").expect("write source");
        fs::write(&dest, b"payload").expect("write dest");
        fs::set_permissions(&source, fs::Permissions::from_mode(0o644)).expect("set source perms");
        // Existing dest mode is generous; chmod must clamp it down.
        fs::set_permissions(&dest, fs::Permissions::from_mode(0o664)).expect("set dest perms");

        let source_meta = fs::metadata(&source).expect("source metadata");
        let modifiers = ChmodModifiers::parse("F600").expect("parse chmod spec");
        let opts = receiver_metadata_opts_no_perms(Some(modifiers));

        apply_file_metadata_with_options(&dest, &source_meta, &opts)
            .expect("apply daemon incoming chmod under --no-perms");

        let mode = fs::metadata(&dest)
            .expect("dest metadata")
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o7777,
            0o600,
            "incoming chmod = F600 under --no-perms must force destination to 0o600 \
             (mode was {:o})",
            mode & 0o7777,
        );
    }

    /// Directories receive the `D+w` clause exclusively: a file-only spec
    /// (`Fo-x`) leaves directory modes untouched under `--no-perms`,
    /// matching upstream `chmod.c:tweak_mode()` clause targeting.
    // upstream: chmod.c:tweak_mode() - F-prefix file-only / D-prefix dir-only.
    #[test]
    fn no_perms_directory_chmod_applies_only_d_clause() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let source = tmp.path().join("source_dir");
        let dest = tmp.path().join("dest_dir");
        fs::create_dir(&source).expect("mkdir source");
        fs::create_dir(&dest).expect("mkdir dest");
        fs::set_permissions(&source, fs::Permissions::from_mode(0o750)).expect("set source perms");
        fs::set_permissions(&dest, fs::Permissions::from_mode(0o750)).expect("set dest perms");

        let source_meta = fs::metadata(&source).expect("source metadata");
        // The `Fo-x` clause is file-only and must NOT touch dir modes.
        let modifiers = ChmodModifiers::parse("Fo-x").expect("parse chmod spec");
        let opts = receiver_metadata_opts_no_perms(Some(modifiers));

        apply_file_metadata_with_options(&dest, &source_meta, &opts)
            .expect("apply chmod to dir under --no-perms");

        let mode = fs::metadata(&dest)
            .expect("dest metadata")
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o7777,
            0o750,
            "file-only chmod clause `Fo-x` must not touch directory modes \
             (mode was {:o})",
            mode & 0o7777,
        );
    }
}

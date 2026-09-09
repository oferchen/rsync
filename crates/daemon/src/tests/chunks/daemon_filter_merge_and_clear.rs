/// End-to-end tests for the daemon `filter` parameter's `merge` and clear forms.
///
/// Both rows were MEASURED against a real rsync 3.5.0 daemon over loopback TCP
/// before they were fixed, with an upstream 3.5.0 client listing the module:
///
/// | `filter = ...`       | upstream 3.5.0 | oc before | oc after  |
/// |----------------------|----------------|-----------|-----------|
/// | `merge FILE`         | hides `bait`   | serves it | hides it  |
/// | `merge /nonexistent` | rc 5           | rc 0      | refused   |
/// | `- bait - ctl !`     | serves `bait`  | hides it  | serves it |
/// | `!`                  | serves `!`     | hides it  | serves it |
///
/// Each fixture plants a bait the predicted-bad rule can actually match plus a
/// control that must survive, so neither direction can pass vacuously.
///
/// # Upstream Reference
///
/// - `clientserver.c:933-935` - `parse_filter_str(&daemon_filter_list,
///   lp_filter(i), rule_template(FILTRULE_WORD_SPLIT), ...)`
/// - `exclude.c:1553-1590` - the `FILTRULE_MERGE_FILE` arm that reads the file
/// - `exclude.c:1541-1550` - `FILTRULE_CLEAR_LIST` pops the accumulated list
#[cfg(unix)]
fn run_daemon_filter_module(
    filter_line: &str,
    populate: &dyn Fn(&Path),
) -> (
    Result<core::client::ClientSummary, core::client::ClientError>,
    PathBuf,
    tempfile::TempDir,
) {
    let temp = tempdir().expect("tempdir");

    let source_dir = temp.path().join("source");
    fs::create_dir_all(&source_dir).expect("create source");
    populate(&source_dir);

    let dest_dir = temp.path().join("dest");
    fs::create_dir(&dest_dir).expect("create dest");

    let config_file = temp.path().join("rsyncd.conf");
    let config_content = format!(
        "[mod]\n\
         path = {}\n\
         read only = true\n\
         use chroot = false\n\
         {filter_line}\n",
        source_dir.display()
    );
    fs::write(&config_file, config_content).expect("write daemon config");

    let (port, held_listener) = allocate_test_port();

    let daemon_config = crate::DaemonConfig::builder()
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

    let rsync_url = format!("rsync://127.0.0.1:{port}/mod/");
    let client_config = core::client::ClientConfig::builder()
        .recursive(true)
        .transfer_args([
            OsString::from(&rsync_url),
            OsString::from(dest_dir.as_os_str()),
        ])
        .build();

    let result = core::client::run_client(client_config);
    let _ = daemon_handle.join();
    (result, dest_dir, temp)
}

/// `filter = merge FILE` applies the rules the named file holds.
///
/// ROW A's red arm: oc built ONE rule, an exclude of the literal pattern
/// `merge <path>`, which matches no file - so `bait.log`, the entry the
/// operator's merge file names, was served.
#[cfg(unix)]
#[test]
fn daemon_filter_merge_applies_the_named_files_rules() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let rules = tempdir().expect("tempdir");
    let rules_file = rules.path().join("rules");
    fs::write(&rules_file, b"- bait.log\n").expect("write merge file");

    let (result, dest, _temp) = run_daemon_filter_module(
        &format!("filter = merge {}", rules_file.display()),
        &|source: &Path| {
            fs::write(source.join("bait.log"), b"bait\n").expect("write bait");
            fs::write(source.join("keep.txt"), b"keep\n").expect("write control");
        },
    );

    result.expect("transfer must succeed");
    assert!(
        dest.join("keep.txt").exists(),
        "the control must survive the merged rules"
    );
    assert!(
        !dest.join("bait.log").exists(),
        "`filter = merge FILE` must apply the file's `- bait.log`"
    );
}

/// A merge file the daemon cannot read REFUSES the module.
///
/// upstream: `XFLG_FATAL_ERRORS` makes the failed open `exit_cleanup(RERR_FILEIO)`
/// (exclude.c:1714-1719). oc served the module with the operator's rules
/// silently absent, which is the same exposure as the row above with no
/// configuration error to notice.
#[cfg(unix)]
#[test]
fn daemon_filter_merge_of_an_unreadable_file_refuses_the_module() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let (result, dest, _temp) = run_daemon_filter_module(
        "filter = merge /nonexistent-oc-rsync-merge-file",
        &|source: &Path| {
            fs::write(source.join("bait.log"), b"bait\n").expect("write bait");
            fs::write(source.join("keep.txt"), b"keep\n").expect("write control");
        },
    );

    assert!(result.is_err(), "an unreadable merge file must refuse");
    assert!(
        !dest.join("keep.txt").exists(),
        "a refused module must serve nothing"
    );
}

/// A trailing `!` token clears the rules written before it.
///
/// ROW B's red arm: oc never opened a token on the `!`, so the value's LAST
/// rule swallowed it and every earlier rule still applied.
///
/// ⚠ THE FIXTURE NEEDS TWO EXCLUDES. With only one, `filter = - bait.log !`
/// collapses on oc-base into a single exclude of the pattern `bait.log !`,
/// which matches nothing - so `bait.log` is served either way and the cell
/// passes on the unfixed tree. MEASURED both ways. The second exclude is what
/// makes the two readings disagree: oc-base opens a token at the `- ` and hides
/// `bait.log` for real, while the clear serves it.
#[cfg(unix)]
#[test]
fn daemon_filter_bang_clears_the_rules_before_it() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let (result, dest, _temp) =
        run_daemon_filter_module("filter = - bait.log - ctl.log !", &|source: &Path| {
            fs::write(source.join("bait.log"), b"bait\n").expect("write bait");
            fs::write(source.join("ctl.log"), b"glue\n").expect("write glue target");
            fs::write(source.join("keep.txt"), b"keep\n").expect("write control");
        });

    result.expect("transfer must succeed");
    assert!(dest.join("keep.txt").exists(), "the control must survive");
    assert!(
        dest.join("bait.log").exists(),
        "the `!` must clear the excludes written before it"
    );
    assert!(
        dest.join("ctl.log").exists(),
        "the `!` must clear the exclude it is glued to as well"
    );
}

/// A bare `!` is the clear rule, never a pattern.
///
/// The bait is a file named `!`: oc's bare-pattern fall-through built an
/// exclude of the literal pattern `!`, which matches exactly that name, so the
/// file vanished from a module upstream serves it from (MEASURED, rc 0 both
/// sides).
#[cfg(unix)]
#[test]
fn daemon_filter_bare_bang_does_not_hide_a_file_named_bang() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let (result, dest, _temp) = run_daemon_filter_module("filter = !", &|source: &Path| {
        fs::write(source.join("!"), b"bait\n").expect("write bait");
        fs::write(source.join("keep.txt"), b"keep\n").expect("write control");
    });

    result.expect("transfer must succeed");
    assert!(dest.join("keep.txt").exists(), "the control must survive");
    assert!(
        dest.join("!").exists(),
        "`filter = !` is a list clear, not an exclude of the name `!`"
    );
}

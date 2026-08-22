/// WHY (class guard, not a regression guard): three daemon entry points - the
/// `--server` stdio session, the inetd session and the remote-shell session -
/// each hand-rolled the module table with a hardcoded null limiter instead of
/// calling the shared builder. Every one of them silently disabled the
/// per-module `max connections` cap, and a module's own `lock file` was ignored
/// outright because the shared builder is what honours it.
///
/// The defect is a SPELLING that any future entry point can reproduce by
/// copying a neighbour, so this pins the spelling rather than the three sites:
/// declaring a null `Option<Arc<ConnectionLimiter>>` is how you opt a transport
/// out of `max connections` without saying so. Build the table through
/// `build_module_runtimes_with_lock_file` instead - it opens the daemon-wide
/// lock file and applies per-module overrides in one place.
///
/// upstream: clientserver.c:791 - `claim_connection()` is called from
/// `rsync_module()`, the single per-module entry every daemon connection
/// reaches. The `am_daemon > 0` test immediately above it guards only the
/// "allowed access" log line, so the cap is deliberately not standalone-only.
#[test]
fn no_daemon_entry_point_hand_rolls_a_null_connection_limiter() {
    // Scoped to PRODUCTION sources. Test fixtures legitimately construct a
    // module runtime with no limiter to exercise the unlimited arm, and test
    // code never serves a connection - the `tests.rs` / `tests/` convention is
    // this crate's own split, so it is the exclusion rule. A consequence worth
    // naming: a hand-roll inside a `#[cfg(test)]` block in a production file
    // would not be seen, which is acceptable because it cannot ship.
    let needle = "Option<Arc<ConnectionLimiter>>";

    fn is_test_source(path: &Path) -> bool {
        path.file_name().is_some_and(|name| name == "tests.rs")
            || path
                .parent()
                .and_then(Path::file_name)
                .is_some_and(|dir| dir == "tests" || dir == "chunks")
    }

    fn collect(dir: &Path, found: &mut Vec<String>, needle: &str) {
        for entry in fs::read_dir(dir).expect("read daemon src dir") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                collect(&path, found, needle);
                continue;
            }
            if path.extension().is_none_or(|ext| ext != "rs") || is_test_source(&path) {
                continue;
            }
            let text = fs::read_to_string(&path).expect("read source file");
            for (index, line) in text.lines().enumerate() {
                if line.contains(needle) && line.contains("None") {
                    found.push(format!("{}:{}", path.display(), index + 1));
                }
            }
        }
    }

    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut found = Vec::new();
    collect(&src, &mut found, needle);

    assert!(
        found.is_empty(),
        "daemon entry points must build their module table through \
         build_module_runtimes_with_lock_file, which honours the daemon-wide \
         `lock file` and per-module overrides; a hand-rolled null limiter \
         disables `max connections` silently. Offending sites: {found:?}",
    );

    // Non-vacuity: the scan must actually be reading this crate's sources.
    // Without this, a wrong root or a broken walk reports the same empty Vec.
    let scanned = fs::read_dir(&src).expect("read daemon src dir").count();
    assert!(scanned > 0, "source scan found no files under {src:?}");
}

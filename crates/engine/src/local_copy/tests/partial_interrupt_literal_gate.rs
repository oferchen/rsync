// An interrupted `--partial` transfer may replace the destination only once
// literal data reached the temp file.
//
// upstream: cleanup.c:159-183 promotes the temp onto its partial destination
// only under `cleanup_got_literal`, which receiver.c:575 sets in the literal
// branch of receive_data(); without it cleanup.c:200-201 unlinks the temp and
// the existing destination survives. Promoting an empty temp would truncate a
// good destination to nothing - an interrupt turned into data loss.

/// Clears the process-global shutdown flag when the test ends.
struct ShutdownFlag;

impl ShutdownFlag {
    fn raise() -> Self {
        fast_io::signal::mark_shutdown();
        Self
    }
}

impl Drop for ShutdownFlag {
    fn drop(&mut self) {
        fast_io::signal::reset_shutdown_for_testing();
    }
}

/// Runs a delta copy of `source` onto `destination` that is interrupted
/// before its first chunk, returning the copy's error kind.
fn interrupt_before_first_chunk(
    source: &Path,
    destination: &Path,
    options: LocalCopyOptions,
) -> LocalCopyErrorKind {
    let operands = vec![
        source.as_os_str().to_os_string(),
        destination.as_os_str().to_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    // `--no-whole-file` routes the existing destination through the delta
    // loop, whose first act is the interrupt poll.
    let _flag = ShutdownFlag::raise();
    plan.execute_with_options(LocalCopyExecution::Apply, options.whole_file(false))
        .expect_err("an interrupted copy must fail")
        .into_kind()
}

fn stale_pair(temp: &Path) -> (PathBuf, PathBuf) {
    let source = temp.join("data.bin");
    let destination = temp.join("data.bin.dest");
    fs::write(&source, b"new source content that differs in size").expect("write source");
    fs::write(&destination, b"existing destination").expect("write destination");
    (source, destination)
}

#[test]
fn partial_interrupt_before_literal_keeps_existing_destination() {
    let temp = tempdir().expect("tempdir");
    let (source, destination) = stale_pair(temp.path());

    let kind = interrupt_before_first_chunk(
        &source,
        &destination,
        LocalCopyOptions::default().partial(true),
    );

    assert!(matches!(kind, LocalCopyErrorKind::Interrupted), "{kind:?}");
    assert_eq!(
        fs::read(&destination).expect("read destination"),
        b"existing destination",
        "a temp holding no literal data must not replace the destination"
    );
    let leftovers: Vec<_> = fs::read_dir(temp.path())
        .expect("read dir")
        .map(|entry| entry.expect("entry").file_name())
        .filter(|name| name.to_string_lossy().starts_with('.'))
        .collect();
    assert!(
        leftovers.is_empty(),
        "the empty temp must be unlinked: {leftovers:?}"
    );
}

#[test]
fn partial_dir_interrupt_before_literal_leaves_no_partial() {
    let temp = tempdir().expect("tempdir");
    let (source, destination) = stale_pair(temp.path());

    let kind = interrupt_before_first_chunk(
        &source,
        &destination,
        LocalCopyOptions::default()
            .partial(true)
            .with_partial_directory(Some(".rsync-partial")),
    );

    assert!(matches!(kind, LocalCopyErrorKind::Interrupted), "{kind:?}");
    assert_eq!(
        fs::read(&destination).expect("read destination"),
        b"existing destination"
    );
    assert!(
        !temp.path().join(".rsync-partial/data.bin.dest").exists(),
        "a temp holding no literal data must not become a partial-dir entry"
    );
}

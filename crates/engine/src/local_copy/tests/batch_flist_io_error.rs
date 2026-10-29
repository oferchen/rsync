// The sender-side `io_error` a `--write-batch` file list carries.
//
// upstream: a local copy forks its sender, so the file list's io_error holds
// only the sender's walk errors (flist.c:2205 opendir, flist.c:1676 readlink);
// a destination chmod the generator fails (generator.c:1907-1910) sets the
// generator's own io_error, which never reaches the batch.

fn batch_recording_context(dir: &Path, options: LocalCopyOptions) -> CopyContext<'static> {
    let config = crate::batch::BatchConfig::new(
        crate::batch::BatchMode::Write,
        dir.join("bf").to_string_lossy().into_owned(),
        29,
    );
    let writer = std::sync::Arc::new(std::sync::Mutex::new(
        crate::batch::BatchWriter::new(config).expect("batch writer"),
    ));
    CopyContext::new(
        LocalCopyExecution::Apply,
        options.batch_writer(Some(writer)),
        None,
        dir.join("dst"),
    )
}

#[cfg(unix)]
#[test]
fn batch_flist_io_error_counts_sender_walk_errors_only() {
    let temp = create_tempdir();
    let mut context = batch_recording_context(temp.path(), LocalCopyOptions::new());
    context.record_generator_io_error();
    assert_eq!(
        context.batch_flist_io_error(),
        0,
        "a generator error is not the sender's"
    );
    context.record_io_error();
    assert_eq!(
        context.batch_flist_io_error(),
        protocol::io_error::IOERR_GENERAL
    );
}

#[test]
fn batch_flist_io_error_counts_dropped_entries() {
    let temp = create_tempdir();
    let mut context = batch_recording_context(temp.path(), LocalCopyOptions::new());
    context.record_flist_io_error();
    assert_eq!(
        context.batch_flist_io_error(),
        protocol::io_error::IOERR_GENERAL
    );
}

#[test]
fn batch_flist_io_error_is_zero_under_ignore_errors() {
    let temp = create_tempdir();
    let options = LocalCopyOptions::new().ignore_errors(true);
    let mut context = batch_recording_context(temp.path(), options);
    context.record_io_error();
    // upstream: flist.c:3066 - `write_int(f, ignore_errors ? 0 : io_error)`.
    assert_eq!(context.batch_flist_io_error(), 0);
}

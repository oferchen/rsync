use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use rsync_core::client::{
    ClientConfig, ClientEventKind, ClientProgressObserver, ClientProgressUpdate, FilterRuleSpec,
    run_client, run_client_with_observer,
};
use tempfile::tempdir;

fn touch(path: &Path, contents: &[u8]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent directories");
    }
    fs::write(path, contents).expect("write fixture file");
}

#[test]
fn run_client_copies_with_delete_and_filters() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");

    fs::create_dir_all(&source_root).expect("source root");
    fs::create_dir_all(&dest_root).expect("dest root");

    touch(&source_root.join("keep.txt"), b"keep");
    touch(&source_root.join("nested/data.bin"), b"payload");
    touch(&source_root.join("remove.tmp"), b"temporary");

    #[cfg(unix)]
    std::os::unix::fs::symlink("keep.txt", source_root.join("keep-link")).expect("symlink");

    touch(&dest_root.join("stale.txt"), b"obsolete");
    touch(&dest_root.join("remove.tmp"), b"old temporary");
    touch(&dest_root.join("protected.txt"), b"protected");

    let mut source_arg = source_root.clone().into_os_string();
    source_arg.push(std::path::MAIN_SEPARATOR.to_string());

    let config = ClientConfig::builder()
        .transfer_args([source_arg, dest_root.clone().into_os_string()])
        .mkpath(true)
        .delete_before(true)
        .delete_excluded(true)
        .add_filter_rule(FilterRuleSpec::exclude("*.tmp"))
        .add_filter_rule(FilterRuleSpec::protect("protected.txt"))
        .permissions(true)
        .times(true)
        .progress(true)
        .stats(true)
        .build();

    let summary = run_client(config).expect("run client");

    assert_eq!(fs::read(dest_root.join("keep.txt")).unwrap(), b"keep");
    assert_eq!(
        fs::read(dest_root.join("nested/data.bin")).unwrap(),
        b"payload"
    );
    #[cfg(unix)]
    {
        let target = fs::read_link(dest_root.join("keep-link")).expect("symlink created");
        assert_eq!(target, PathBuf::from("keep.txt"));
    }
    assert!(
        !dest_root.join("remove.tmp").exists(),
        "excluded files deleted"
    );
    assert!(
        dest_root.join("protected.txt").exists(),
        "protected entries are preserved"
    );
    assert!(
        !dest_root.join("stale.txt").exists(),
        "stale entries removed during delete-before"
    );

    assert!(summary.files_copied() >= 2);
    assert!(summary.items_deleted() >= 1);
    assert!(summary.bytes_copied() > 0);
}

#[derive(Default)]
struct RecordingObserver {
    updates: Vec<(PathBuf, ClientEventKind, bool, Option<u64>, u64)>,
}

impl ClientProgressObserver for RecordingObserver {
    fn on_progress(&mut self, update: &ClientProgressUpdate) {
        self.updates.push((
            update.event().relative_path().to_path_buf(),
            update.event().kind().clone(),
            update.is_final(),
            update.total_bytes(),
            update.overall_transferred(),
        ));
    }
}

#[test]
fn progress_observer_reports_transfers() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("src");
    let dest_root = temp.path().join("dst");

    touch(&source_root.join("first.bin"), b"1234567890");
    touch(&source_root.join("nested/second.bin"), b"abcdefghij");

    let mut source_arg = source_root.clone().into_os_string();
    source_arg.push(std::path::MAIN_SEPARATOR.to_string());

    let config = ClientConfig::builder()
        .transfer_args([source_arg, dest_root.clone().into_os_string()])
        .mkpath(true)
        .progress(true)
        .stats(true)
        .build();

    let mut observer = RecordingObserver::default();
    let summary = run_client_with_observer(config, Some(&mut observer)).expect("run client");

    assert_eq!(summary.files_copied(), 2);
    let data_updates: Vec<_> = observer
        .updates
        .iter()
        .filter(|(_, kind, ..)| matches!(kind, ClientEventKind::DataCopied))
        .collect();
    assert!(data_updates.len() >= 2, "expected data copy updates");

    let mut seen_nested = false;
    let mut last_transferred = 0;
    let mut completions: HashMap<PathBuf, bool> = HashMap::new();
    for (path, _, final_update, total_bytes, transferred) in data_updates {
        let file_name = path
            .file_name()
            .expect("progress events reference concrete files");
        assert!(
            file_name.to_string_lossy().ends_with(".bin"),
            "unexpected event path: {path:?}"
        );
        assert!(total_bytes.is_some(), "byte counts emitted for each file");
        assert!(
            *transferred >= last_transferred,
            "progress increments transferred bytes"
        );
        last_transferred = *transferred;
        if path
            .components()
            .any(|component| component.as_os_str() == "nested")
        {
            seen_nested = true;
        }
        completions
            .entry(path.clone())
            .and_modify(|done| *done |= *final_update)
            .or_insert(*final_update);
    }
    assert!(seen_nested, "progress includes nested entries");
    assert!(
        completions.values().filter(|done| **done).count() >= 2,
        "all files eventually report completion"
    );
}

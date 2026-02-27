//! Async file job producer for the transfer pipeline.
//!
//! Iterates a [`FileList`] and dispatches [`FileJob`] values through a bounded
//! `tokio::sync::mpsc` channel. The channel capacity provides natural backpressure:
//! when the consumer falls behind, `send().await` suspends the producer task.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::mpsc;

use super::job::{FileJob, FileList};

/// Produces [`FileJob`] values from a [`FileList`] and sends them through a channel.
///
/// Iterates all entries in the file list, creating a `FileJob` for each regular
/// file. Directories and other non-file entries are skipped since they don't
/// require delta transfers.
///
/// The function returns when all jobs have been sent or the receiver is dropped
/// (indicating the consumer has shut down). Dropping the `tx` half after iteration
/// signals end-of-stream to the consumer.
///
/// # Arguments
///
/// * `file_list` - Immutable, sorted file list shared via `Arc`.
/// * `dest_dir` - Base destination directory joined with each entry's path.
/// * `tx` - Bounded channel sender. Backpressure suspends this task when full.
pub async fn produce_file_jobs(
    file_list: &FileList,
    dest_dir: &Path,
    tx: mpsc::Sender<FileJob>,
) -> u64 {
    let entries = file_list.shared();
    let mut dispatched = 0u64;

    for (ndx, entry) in entries.iter().enumerate() {
        if !entry.is_file() {
            continue;
        }

        let dest_path = dest_dir.join(entry.path());
        let entry_arc = Arc::new(entry.clone());

        #[allow(clippy::cast_possible_truncation)]
        let job = FileJob::new(ndx as u32, dest_path, entry_arc);

        if tx.send(job).await.is_err() {
            // Consumer dropped — stop producing.
            break;
        }
        dispatched += 1;
    }

    dispatched
}

/// Creates destination path for a file entry.
#[must_use]
pub fn make_dest_path(dest_dir: &Path, entry_path: &Path) -> PathBuf {
    dest_dir.join(entry_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::flist::FileEntry;

    fn make_file(name: &str, size: u64) -> FileEntry {
        FileEntry::new_file(name.into(), size, 0o644)
    }

    fn make_dir(name: &str) -> FileEntry {
        FileEntry::new_directory(name.into(), 0o755)
    }

    #[tokio::test]
    async fn produce_empty_list() {
        let list = FileList::new(Vec::new());
        let (tx, mut rx) = mpsc::channel(8);

        let count = produce_file_jobs(&list, Path::new("/dst"), tx).await;

        assert_eq!(count, 0);
        assert!(rx.recv().await.is_none());
    }

    #[tokio::test]
    async fn produce_skips_directories() {
        let entries = vec![make_dir("subdir"), make_file("a.txt", 100)];
        let list = FileList::new(entries);
        let (tx, mut rx) = mpsc::channel(8);

        let count = produce_file_jobs(&list, Path::new("/dst"), tx).await;

        assert_eq!(count, 1);
        let job = rx.recv().await.expect("one job");
        assert_eq!(job.ndx(), 1); // index 1 (directory at 0 was skipped)
        assert!(rx.recv().await.is_none());
    }

    #[tokio::test]
    async fn produce_all_regular_files() {
        let entries = vec![
            make_file("a.txt", 10),
            make_file("b.txt", 20),
            make_file("c.txt", 30),
        ];
        let list = FileList::new(entries);
        let (tx, mut rx) = mpsc::channel(8);

        let count = produce_file_jobs(&list, Path::new("/dst"), tx).await;

        assert_eq!(count, 3);
        for expected_ndx in 0..3u32 {
            let job = rx.recv().await.expect("job");
            assert_eq!(job.ndx(), expected_ndx);
        }
        assert!(rx.recv().await.is_none());
    }

    #[tokio::test]
    async fn produce_builds_dest_path() {
        let entries = vec![make_file("sub/file.txt", 50)];
        let list = FileList::new(entries);
        let (tx, mut rx) = mpsc::channel(8);

        produce_file_jobs(&list, Path::new("/dst"), tx).await;

        let job = rx.recv().await.expect("job");
        assert_eq!(job.dest_path(), Path::new("/dst/sub/file.txt"));
    }

    #[tokio::test]
    async fn produce_stops_on_receiver_drop() {
        let entries: Vec<_> = (0..100)
            .map(|i| make_file(&format!("file_{i}.txt"), 10))
            .collect();
        let list = FileList::new(entries);
        let (tx, rx) = mpsc::channel(1);

        // Drop the receiver immediately — producer should stop gracefully.
        drop(rx);
        let count = produce_file_jobs(&list, Path::new("/dst"), tx).await;

        // Should have dispatched 0 or 1 (channel capacity 1, receiver dropped).
        assert!(count <= 1);
    }

    #[tokio::test]
    async fn make_dest_path_joins_correctly() {
        let path = make_dest_path(Path::new("/backup"), Path::new("docs/readme.txt"));
        assert_eq!(path, PathBuf::from("/backup/docs/readme.txt"));
    }
}

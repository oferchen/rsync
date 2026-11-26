/// Largest literal token a batch records (upstream: rsync.h:159 `CHUNK_SIZE`).
const BATCH_LITERAL_CHUNK: usize = protocol::wire::CHUNK_SIZE;

/// How a whole-file copy strategy moves the file's data.
///
/// A `--write-batch` run must record every byte of a new file as literal
/// tokens, and the only way to get the bytes is to read them. The read loop
/// records the tokens from its own read and needs no entry here; a mover that
/// moves no data (a clone) leaves one capture read; one that moves the bytes
/// without them reaching user space would force the capture to read them a
/// second time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DataMover {
    /// The destination shares the source's extents (`FICLONE`, `clonefile`):
    /// no data is read or written. Only the platforms with a clone strategy
    /// have it.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    Clone,
    /// The kernel copies the bytes without them reaching user space
    /// (`copy_file_range`, the io_uring copy, `CopyFileExW`).
    Kernel,
}

impl CopyContext<'_> {
    /// Reports whether `mover` may copy a whole file in this run.
    ///
    /// While a batch is recorded a [`DataMover::Kernel`] copy is refused, so
    /// the read loop's own read writes the literal tokens and the file is read
    /// once, as rsync 3.5.2's sender reads it once to send its tokens
    /// (match.c:480-486 `match_sums()` with no sums, token.c:317-327
    /// `simple_send_token()`). A clone stays allowed: it moves no data, so the
    /// capture after it is still the file's only read, and the blocks stay
    /// shared.
    pub(crate) const fn mover_allowed(&self, mover: DataMover) -> bool {
        match mover {
            DataMover::Kernel => self.batch_delta_buf.is_none(),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            DataMover::Clone => true,
        }
    }
}

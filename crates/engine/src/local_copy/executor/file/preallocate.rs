//! File preallocation for reducing fragmentation during writes.
//!
//! Uses `fallocate(2)` on Linux to reserve contiguous disk space before
//! writing file data, falling back to a no-op on other platforms.
//!
//! upstream: receiver.c - preallocate support via --preallocate

use std::fs;
#[cfg(unix)]
use std::io;
use std::path::Path;

#[cfg(unix)]
use rustix::{
    fd::AsFd,
    fs::{FallocateFlags, fallocate},
    io::Errno,
};

use crate::local_copy::LocalCopyError;

/// Which `fallocate(2)` reservation upstream's `do_fallocate()` makes.
///
/// `FALLOC_FL_KEEP_SIZE` reserves blocks without moving the file's logical end,
/// but a hole-punch can only deallocate blocks that lie *inside* the file's
/// size - with `KEEP_SIZE` the reserved blocks sit beyond EOF and the punch
/// silently does nothing, leaving the file fully allocated. So when holes will
/// also be punched (`--sparse`), upstream reserves at full size instead.
/// upstream: syscall.c:2597 do_fallocate() - `int opts = (inplace ||
/// preallocate_files) && sparse_files <= 0 ? DO_FALLOC_OPTIONS : 0;`
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Reservation {
    /// `opts == FALLOC_FL_KEEP_SIZE`: no holes will be punched.
    KeepSize,
    /// `opts == 0`: `--sparse` is active, so the extent must lie inside the
    /// file's size for `do_punch_hole()` to be able to deallocate it.
    FullSize,
}

impl Reservation {
    /// Selects the reservation upstream would make for this sparse setting.
    /// upstream: syscall.c:2597 - `sparse_files <= 0` is what selects KEEP_SIZE.
    pub(crate) const fn for_sparse(sparse: bool) -> Self {
        if sparse {
            Self::FullSize
        } else {
            Self::KeepSize
        }
    }
}

/// The reservation actually available on this platform.
///
/// `FALLOC_FL_KEEP_SIZE` is Linux-only; elsewhere every reservation is
/// full-size, which is upstream's `opts == 0` path and reports its value.
#[cfg(unix)]
const fn available(reservation: Reservation) -> Reservation {
    #[cfg(target_os = "linux")]
    {
        reservation
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = reservation;
        Reservation::FullSize
    }
}

/// Preallocates disk space for the destination file when requested and needed.
///
/// `reservation` is `None` when `--preallocate` is off. Preallocation is also
/// skipped when `total_len` is zero or the file already spans `total_len`
/// bytes.
///
/// Returns the value upstream `do_fallocate()` feeds into `preallocated_len`:
/// the reserved length for [`Reservation::KeepSize`], and `st_blocks * S_BLKSIZE`
/// for [`Reservation::FullSize`]. The sparse writer compares an interior zero
/// run's start against it to choose `do_punch_hole()` over a plain `lseek()`, so
/// a stray `0` here silently leaves `--preallocate --sparse` files fully
/// allocated - the exact regression upstream records at syscall.c:2622.
/// upstream: syscall.c:2589 do_fallocate(); receiver.c:479
/// `preallocated_len = do_fallocate(fd, 0, total_size)`
pub(crate) fn maybe_preallocate_destination(
    file: &mut fs::File,
    path: &Path,
    total_len: u64,
    existing_bytes: u64,
    reservation: Option<Reservation>,
) -> Result<u64, LocalCopyError> {
    let Some(reservation) = reservation else {
        return Ok(0);
    };
    if total_len == 0 || total_len <= existing_bytes {
        return Ok(0);
    }

    preallocate_destination_file(file, path, total_len, reservation)
}

fn preallocate_destination_file(
    file: &mut fs::File,
    path: &Path,
    total_len: u64,
    reservation: Reservation,
) -> Result<u64, LocalCopyError> {
    #[cfg(unix)]
    {
        if total_len == 0 {
            return Ok(0);
        }

        if total_len > i64::MAX as u64 {
            return Err(LocalCopyError::io(
                "preallocate destination file",
                path.to_path_buf(),
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "preallocation size exceeds platform limit",
                ),
            ));
        }

        // upstream: syscall.c:2601-2604 - the reservation is deliberately made
        // one byte off the requested size ("make the length not match the
        // desired length"), and do_fallocate() reports that same perturbed
        // length. total_len > 0 is guaranteed above, so this cannot underflow.
        let length = if total_len & 1 == 1 {
            total_len + 1
        } else {
            total_len - 1
        };

        let reservation = available(reservation);
        let fd = file.as_fd();
        // upstream: syscall.c:2584 DO_FALLOC_OPTIONS = FALLOC_FL_KEEP_SIZE, but
        // syscall.c:2597 selects it only when no holes will be punched. KEEP_SIZE
        // leaves the file's apparent size (st_size) untouched - it grows only as
        // data is written, preserving the sparse-until-written appearance
        // observable mid-transfer via stat / du --apparent-size - but the blocks
        // it reserves then sit BEYOND EOF, where do_punch_hole() cannot reach
        // them. Under --sparse we therefore reserve at full size instead so the
        // sparse writer can punch the zero runs back out.
        #[cfg(target_os = "linux")]
        let flags = match reservation {
            Reservation::KeepSize => FallocateFlags::KEEP_SIZE,
            Reservation::FullSize => FallocateFlags::empty(),
        };
        // Non-Linux lacks KEEP_SIZE; `available()` already forced FullSize.
        #[cfg(not(target_os = "linux"))]
        let flags = FallocateFlags::empty();
        match fallocate(fd, flags, 0, length) {
            Ok(()) => Ok(match reservation {
                // upstream: syscall.c:2622-2629 - with KEEP_SIZE the blocks for
                // [0, length) are reserved even though the file size stays put,
                // so report that reserved length. Reporting 0 here is upstream's
                // pre-3.5.0 behaviour and is precisely why `--preallocate
                // --sparse` stopped producing sparse files: every zero run then
                // compares `>= 0` and is seeked over rather than punched,
                // leaving the whole reserved extent allocated.
                Reservation::KeepSize => length,
                // upstream: syscall.c:2616-2620 - opts == 0 reports the
                // resulting allocation, falling back to `length` if fstat fails.
                Reservation::FullSize => allocated_bytes(file).unwrap_or(length),
            }),
            // KEEP_SIZE unavailable at runtime: fall back to a size-extending
            // reservation (equivalent to upstream's opts == 0 path) and report the
            // resulting allocation so the sparse writer punches within it.
            Err(Errno::OPNOTSUPP | Errno::NOSYS | Errno::INVAL) => {
                file.set_len(length).map_err(|error| {
                    LocalCopyError::io("preallocate destination file", path, error)
                })?;
                Ok(allocated_bytes(file).unwrap_or(length))
            }
            Err(errno) => Err(LocalCopyError::io(
                "preallocate destination file",
                path.to_path_buf(),
                io::Error::from_raw_os_error(errno.raw_os_error()),
            )),
        }
    }

    #[cfg(not(unix))]
    {
        // No fallocate: the only reservation available extends the file, which
        // is upstream's `opts == 0` shape, so report the reserved length.
        let _ = reservation;
        if total_len == 0 {
            return Ok(0);
        }

        file.set_len(total_len)
            .map_err(|error| LocalCopyError::io("preallocate destination file", path, error))?;
        Ok(total_len)
    }
}

/// Returns the number of bytes currently allocated on disk for `file`
/// (`st_blocks * 512`), mirroring upstream `st.st_blocks * S_BLKSIZE`.
#[cfg(unix)]
fn allocated_bytes(file: &fs::File) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    file.metadata().ok().map(|meta| meta.blocks() * 512)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;

    #[test]
    fn maybe_preallocate_disabled_does_nothing() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("test.txt");
        let mut file = fs::File::create(&path).expect("create file");

        // When disabled, should succeed without preallocating
        let result = maybe_preallocate_destination(&mut file, &path, 1000, 0, None);
        assert!(result.is_ok());

        // File should remain empty
        let metadata = fs::metadata(&path).expect("metadata");
        assert_eq!(metadata.len(), 0);
    }

    #[test]
    fn maybe_preallocate_zero_length_does_nothing() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("test.txt");
        let mut file = fs::File::create(&path).expect("create file");

        // When total_len is 0, should succeed without preallocating
        let result =
            maybe_preallocate_destination(&mut file, &path, 0, 0, Some(Reservation::KeepSize));
        assert!(result.is_ok());
    }

    #[test]
    fn maybe_preallocate_already_large_enough_does_nothing() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("test.txt");
        let mut file = fs::File::create(&path).expect("create file");
        file.write_all(b"existing content").expect("write");
        file.flush().expect("flush");

        let existing_bytes = 16; // Length of "existing content"
        // When total_len <= existing_bytes, should succeed without preallocating
        let result = maybe_preallocate_destination(
            &mut file,
            &path,
            10,
            existing_bytes,
            Some(Reservation::KeepSize),
        );
        assert!(result.is_ok());
    }

    #[test]
    fn maybe_preallocate_enabled_preallocates_file() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("test.txt");
        let mut file = fs::File::create(&path).expect("create file");

        // When enabled and total_len > existing_bytes, should preallocate
        let result =
            maybe_preallocate_destination(&mut file, &path, 1000, 0, Some(Reservation::KeepSize));
        assert!(result.is_ok());

        let metadata = fs::metadata(&path).expect("metadata");
        // Linux reserves blocks with FALLOC_FL_KEEP_SIZE, leaving the apparent
        // size untouched; other platforms extend the file to the requested size.
        #[cfg(target_os = "linux")]
        assert_eq!(metadata.len(), 0, "KEEP_SIZE must not extend apparent size");
        // Other Unix: fallocate is unavailable, so the fallback extends the
        // file to upstream's deliberately-perturbed `length`
        // (upstream: syscall.c:2601-2604; receiver.c:652 trims the excess).
        #[cfg(all(unix, not(target_os = "linux")))]
        assert_eq!(metadata.len(), 999);
        // Windows has no fallocate at all: the file is extended to exactly
        // total_len, so there is no over-preallocation and the perturbation
        // upstream applies to a reservation does not apply here.
        #[cfg(not(unix))]
        assert_eq!(metadata.len(), 1000);
    }

    #[test]
    fn preallocate_destination_file_sets_length() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("test.txt");
        let mut file = fs::File::create(&path).expect("create file");

        let result = preallocate_destination_file(&mut file, &path, 2048, Reservation::KeepSize);
        assert!(result.is_ok());

        let metadata = fs::metadata(&path).expect("metadata");
        // KEEP_SIZE (Linux) reserves blocks without extending the apparent size.
        #[cfg(target_os = "linux")]
        assert_eq!(metadata.len(), 0, "KEEP_SIZE must not extend apparent size");
        // Other Unix: fallocate is unavailable, so the fallback extends the
        // file to upstream's deliberately-perturbed `length`
        // (upstream: syscall.c:2601-2604; receiver.c:652 trims the excess).
        #[cfg(all(unix, not(target_os = "linux")))]
        assert_eq!(metadata.len(), 2047);
        // Windows has no fallocate at all: the file is extended to exactly
        // total_len, so there is no over-preallocation and the perturbation
        // upstream applies to a reservation does not apply here.
        #[cfg(not(unix))]
        assert_eq!(metadata.len(), 2048);
    }

    #[test]
    fn preallocate_destination_file_zero_length_succeeds() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("test.txt");
        let mut file = fs::File::create(&path).expect("create file");

        let result = preallocate_destination_file(&mut file, &path, 0, Reservation::KeepSize);
        assert!(result.is_ok());

        let metadata = fs::metadata(&path).expect("metadata");
        assert_eq!(metadata.len(), 0);
    }

    /// Verify that preallocation rejects sizes exceeding the i64::MAX platform
    /// limit on Unix.  The `fallocate()` offset parameter is a signed 64-bit
    /// integer, so values above `i64::MAX` must be rejected before the syscall.
    #[cfg(unix)]
    #[test]
    fn preallocate_rejects_size_exceeding_i64_max() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("overflow.bin");
        let mut file = fs::File::create(&path).expect("create file");

        let oversized = (i64::MAX as u64) + 1;
        let result =
            preallocate_destination_file(&mut file, &path, oversized, Reservation::KeepSize);
        assert!(result.is_err(), "expected error for size > i64::MAX");

        let error = result.unwrap_err();
        let msg = format!("{error}");
        assert!(
            msg.contains("platform limit"),
            "error should mention platform limit, got: {msg}"
        );
    }

    /// Verify that the boundary value `i64::MAX` itself is not rejected
    /// (the syscall may still fail for other reasons, but the size check
    /// should pass).
    #[cfg(unix)]
    #[test]
    fn preallocate_accepts_i64_max_boundary() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("boundary.bin");
        let mut file = fs::File::create(&path).expect("create file");

        // i64::MAX is a valid argument to fallocate(), though the OS will
        // likely reject it due to disk space.  We only verify that our
        // size guard does not reject it prematurely.
        let boundary = i64::MAX as u64;
        let result =
            preallocate_destination_file(&mut file, &path, boundary, Reservation::KeepSize);
        // The result may be Err (ENOSPC or similar) but not our "platform limit" error
        if let Err(ref error) = result {
            let msg = format!("{error}");
            assert!(
                !msg.contains("platform limit"),
                "i64::MAX should pass the size guard, got: {msg}"
            );
        }
    }

    /// Verify that preallocation of a large file (1 MiB) actually allocates
    /// disk blocks on Linux.
    #[cfg(target_os = "linux")]
    #[test]
    fn preallocate_large_file_allocates_blocks() {
        use std::os::unix::fs::MetadataExt;

        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("large.bin");
        let mut file = fs::File::create(&path).expect("create file");

        let one_mib = 1024 * 1024;
        let result = preallocate_destination_file(&mut file, &path, one_mib, Reservation::KeepSize);
        assert!(result.is_ok());

        let metadata = fs::metadata(&path).expect("metadata");
        // FALLOC_FL_KEEP_SIZE reserves the blocks but leaves st_size at 0.
        assert_eq!(metadata.len(), 0, "KEEP_SIZE must not extend apparent size");

        // On Linux, fallocate() should reserve disk blocks.  The 512-byte
        // block count should be at least file_size / 512.  Some filesystems
        // may allocate slightly more due to alignment, but never less.
        let expected_min_blocks: u64 = one_mib / 512;
        assert!(
            metadata.blocks() >= expected_min_blocks,
            "expected at least {} blocks, got {}",
            expected_min_blocks,
            metadata.blocks()
        );
    }

    /// Verify `maybe_preallocate_destination` mirrors upstream `do_fallocate()`:
    /// the `FALLOC_FL_KEEP_SIZE` path reports the RESERVED LENGTH, never 0.
    ///
    /// `preallocated_len` is what `flush_sparse_hole()` compares an interior zero
    /// run's start against, so a 0 makes every run compare `>= 0` and be seeked
    /// over instead of punched - leaving a `--preallocate --sparse` file fully
    /// allocated. Upstream records that exact regression in its own source: "a
    /// stray 0 here, from 2019's switch to KEEP_SIZE, is why --preallocate
    /// --sparse stopped producing sparse files".
    // upstream: syscall.c:2629 do_fallocate() - `return length;`
    // upstream: fileio.c:84 flush_sparse_hole() - `sparse_past_write >= preallocated_len`
    #[cfg(target_os = "linux")]
    #[test]
    fn keep_size_reports_the_reserved_length_not_zero() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("prealloc_len.bin");
        let mut file = fs::File::create(&path).expect("create file");

        let one_mib: u64 = 1024 * 1024;
        let reserved = maybe_preallocate_destination(
            &mut file,
            &path,
            one_mib,
            0,
            Some(Reservation::KeepSize),
        )
        .expect("preallocate");

        // upstream: syscall.c:2601-2604 - the reservation is one byte off the request.
        assert_eq!(
            reserved,
            one_mib - 1,
            "KEEP_SIZE must report its reserved length so zero runs are punched"
        );
        // KEEP_SIZE leaves the apparent size alone, which is exactly why the
        // reserved blocks sit beyond EOF and a punch cannot reach them.
        let metadata = fs::metadata(&path).expect("metadata");
        assert_eq!(metadata.len(), 0, "KEEP_SIZE must not extend apparent size");
    }

    /// Non-vacuity companion for the pin above: under `--sparse` upstream drops
    /// `KEEP_SIZE` entirely (`opts = 0`) and reserves at full size, so the extent
    /// lies INSIDE the file's size where `do_punch_hole()` can deallocate it. An
    /// implementation that ignored the reservation and always used `KEEP_SIZE`
    /// would still satisfy the length assertion above, but fails here.
    // upstream: syscall.c:2597 - `... && sparse_files <= 0 ? DO_FALLOC_OPTIONS : 0`
    #[cfg(target_os = "linux")]
    #[test]
    fn sparse_reservation_lands_inside_the_file_size() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("sparse_len.bin");
        let mut file = fs::File::create(&path).expect("create file");

        let one_mib: u64 = 1024 * 1024;
        maybe_preallocate_destination(&mut file, &path, one_mib, 0, Some(Reservation::FullSize))
            .expect("preallocate");

        let metadata = fs::metadata(&path).expect("metadata");
        assert_eq!(
            metadata.len(),
            one_mib - 1,
            "a punchable reservation must extend the file's size, not sit beyond EOF"
        );
    }

    /// Preallocation that never ran reports no reserved extent, so upstream's
    /// `else` arm (`preallocated_len = 0`) is what the sparse writer sees.
    // upstream: receiver.c:492 - `preallocated_len = 0;`
    #[test]
    fn disabled_preallocation_reports_no_extent() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("skip.bin");
        let mut file = fs::File::create(&path).expect("create file");

        let skipped =
            maybe_preallocate_destination(&mut file, &path, 1024 * 1024, 0, None).expect("skip");
        assert_eq!(skipped, 0, "disabled preallocation should report 0 length");
    }

    /// Verify that disabled preallocation does not allocate extra blocks.
    #[cfg(target_os = "linux")]
    #[test]
    fn disabled_preallocate_does_not_reserve_blocks() {
        use std::os::unix::fs::MetadataExt;

        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("no_prealloc.bin");
        let mut file = fs::File::create(&path).expect("create file");

        let result = maybe_preallocate_destination(&mut file, &path, 1024 * 1024, 0, None);
        assert!(result.is_ok());

        let metadata = fs::metadata(&path).expect("metadata");
        assert_eq!(metadata.len(), 0);
        assert_eq!(
            metadata.blocks(),
            0,
            "disabled preallocate should not reserve any blocks"
        );
    }

    /// Verify that `maybe_preallocate_destination` handles the exact boundary
    /// where `total_len == existing_bytes` by skipping preallocation.
    #[test]
    fn maybe_preallocate_exact_boundary_does_nothing() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("boundary.txt");
        let mut file = fs::File::create(&path).expect("create file");
        file.write_all(b"12345").expect("write");
        file.flush().expect("flush");

        // total_len == existing_bytes: should skip
        let result =
            maybe_preallocate_destination(&mut file, &path, 5, 5, Some(Reservation::KeepSize));
        assert!(result.is_ok());
    }

    /// Verify preallocation works when writing to an already opened file
    /// (simulating the inplace write pattern).
    #[test]
    fn preallocate_works_with_writable_file() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("inplace.bin");

        // Open in read-write mode (like --inplace)
        let mut file = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .expect("open file");

        let result =
            maybe_preallocate_destination(&mut file, &path, 4096, 0, Some(Reservation::KeepSize));
        assert!(result.is_ok());

        let metadata = fs::metadata(&path).expect("metadata");
        // KEEP_SIZE (Linux) leaves the apparent size at 0; writes then grow it as
        // data lands, exactly as upstream's receiver observes it mid-transfer.
        #[cfg(target_os = "linux")]
        assert_eq!(metadata.len(), 0, "KEEP_SIZE must not extend apparent size");
        // Other Unix: fallocate is unavailable, so the fallback extends the
        // file to upstream's deliberately-perturbed `length`
        // (upstream: syscall.c:2601-2604; receiver.c:652 trims the excess).
        #[cfg(all(unix, not(target_os = "linux")))]
        assert_eq!(metadata.len(), 4095);
        // Windows has no fallocate at all: the file is extended to exactly
        // total_len, so there is no over-preallocation and the perturbation
        // upstream applies to a reservation does not apply here.
        #[cfg(not(unix))]
        assert_eq!(metadata.len(), 4096);

        // Write some content to the preallocated space
        file.write_all(b"hello preallocated world").expect("write");
        file.flush().expect("flush");

        let metadata = fs::metadata(&path).expect("metadata after write");
        // On Linux the size now reflects the 24 bytes written; elsewhere the
        // earlier size-extending reservation still governs the length.
        #[cfg(target_os = "linux")]
        assert_eq!(metadata.len(), 24, "size grows only as data is written");
        // Other Unix: fallocate is unavailable, so the fallback extends the
        // file to upstream's deliberately-perturbed `length`
        // (upstream: syscall.c:2601-2604; receiver.c:652 trims the excess).
        #[cfg(all(unix, not(target_os = "linux")))]
        assert_eq!(metadata.len(), 4095);
        // Windows has no fallocate at all: the file is extended to exactly
        // total_len, so there is no over-preallocation and the perturbation
        // upstream applies to a reservation does not apply here.
        #[cfg(not(unix))]
        assert_eq!(metadata.len(), 4096);
    }

    /// Verify that preallocating a file that already has some content reserves
    /// the requested extent (the append offset scenario). On Linux KEEP_SIZE
    /// leaves the apparent size at what was already written; other platforms
    /// extend it to the requested size.
    #[test]
    fn preallocate_reserves_for_partially_written_file() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("partial.bin");
        let mut file = fs::File::create(&path).expect("create file");
        file.write_all(&[0xAA; 100]).expect("write initial");
        file.flush().expect("flush");

        // Preallocate to 4096 even though 100 bytes are written.
        // existing_bytes=100 < total_len=4096, so preallocation should happen.
        let result =
            maybe_preallocate_destination(&mut file, &path, 4096, 100, Some(Reservation::KeepSize));
        assert!(result.is_ok());

        let metadata = fs::metadata(&path).expect("metadata");
        #[cfg(target_os = "linux")]
        assert_eq!(
            metadata.len(),
            100,
            "KEEP_SIZE preserves the written length"
        );
        // Other Unix: fallocate is unavailable, so the fallback extends the
        // file to upstream's deliberately-perturbed `length`
        // (upstream: syscall.c:2601-2604; receiver.c:652 trims the excess).
        #[cfg(all(unix, not(target_os = "linux")))]
        assert_eq!(metadata.len(), 4095);
        // Windows has no fallocate at all: the file is extended to exactly
        // total_len, so there is no over-preallocation and the perturbation
        // upstream applies to a reservation does not apply here.
        #[cfg(not(unix))]
        assert_eq!(metadata.len(), 4096);
    }

    /// Verify preallocation with a variety of sizes including small files
    /// that might not be worth preallocating in practice, but should still
    /// succeed when the feature is enabled.
    #[test]
    fn preallocate_small_file_succeeds() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("tiny.bin");
        let mut file = fs::File::create(&path).expect("create file");

        // Even a 1-byte preallocation should succeed
        let result =
            maybe_preallocate_destination(&mut file, &path, 1, 0, Some(Reservation::KeepSize));
        assert!(result.is_ok());

        let metadata = fs::metadata(&path).expect("metadata");
        #[cfg(target_os = "linux")]
        assert_eq!(metadata.len(), 0, "KEEP_SIZE must not extend apparent size");
        // Other Unix: fallocate is unavailable, so the fallback extends the
        // file to upstream's deliberately-perturbed `length`
        // (upstream: syscall.c:2601-2604; receiver.c:652 trims the excess).
        #[cfg(all(unix, not(target_os = "linux")))]
        assert_eq!(metadata.len(), 2);
        // Windows has no fallocate at all: the file is extended to exactly
        // total_len, so there is no over-preallocation and the perturbation
        // upstream applies to a reservation does not apply here.
        #[cfg(not(unix))]
        assert_eq!(metadata.len(), 1);
    }

    /// Regression guard for the KEEP_SIZE behavior-fidelity fix. Upstream's
    /// do_fallocate() reserves blocks with FALLOC_FL_KEEP_SIZE, so the apparent
    /// size (st_size) must NOT jump to total_len while the transfer is still
    /// writing - it grows only as data lands. Before the fix, a plain fallocate
    /// (or the set_len fallback) extended st_size to total_len immediately,
    /// observable via stat / du --apparent-size mid-transfer.
    // upstream: syscall.c:2584 DO_FALLOC_OPTIONS = FALLOC_FL_KEEP_SIZE
    #[cfg(target_os = "linux")]
    #[test]
    fn preallocate_keep_size_does_not_extend_apparent_size() {
        use std::os::unix::fs::MetadataExt;

        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("keep_size.bin");
        let mut file = fs::File::create(&path).expect("create file");

        let total_len: u64 = 1024 * 1024;
        let reserved = maybe_preallocate_destination(
            &mut file,
            &path,
            total_len,
            0,
            Some(Reservation::KeepSize),
        )
        .expect("prealloc");

        let metadata = fs::metadata(&path).expect("metadata");
        // The apparent size must stay at 0: KEEP_SIZE reserves blocks without
        // extending st_size to the eventual length.
        assert_eq!(
            metadata.len(),
            0,
            "apparent size must not be prematurely extended to total_len"
        );
        // Yet the blocks are reserved (unless the filesystem lacks fallocate, in
        // which case the fallback set_len would have reported total_len as the
        // length above - which it did not).
        assert!(
            reserved == total_len - 1 || metadata.blocks() * 512 >= total_len - 1,
            "blocks should be reserved for the eventual length (reserved={reserved}, blocks*512={})",
            metadata.blocks() * 512
        );
    }
}

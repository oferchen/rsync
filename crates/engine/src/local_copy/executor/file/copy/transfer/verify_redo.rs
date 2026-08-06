//! The `--append-verify` append -> verify -> retain -> redo cycle.
//!
//! Upstream gets this for free on a local transfer because "local" is just a
//! client and a server exchanging the ordinary protocol over a socketpair
//! (main.c:1468 sets `local_server`, main.c:649-655 forks `local_child`, and
//! main.c:1050-1132 forks again so `recv_files()` and `generate_files()` run
//! concurrently). The local executor is a separate implementation of that
//! transfer, so it has to reproduce the semantics explicitly:
//!
//! 1. **Append first.** The sender jumps `last_match` to the destination's
//!    length and zeroes the block count (match.c:372-391), and the generator
//!    writes a sum header with no block sums (generator.c:787), so pass one is
//!    always a pure append.
//! 2. **Verify the whole file.** `receive_data()` compares the sender's
//!    whole-file checksum with the receiver's (receiver.c:517-519). With
//!    `--append-verify` both sides fold the pre-existing prefix into that sum
//!    (match.c:373-386, receiver.c:357-371) and then the identical appended
//!    tail, so the comparison is exactly "do the two prefixes agree".
//! 3. **Retain the result.** `--append` implies `--inplace`
//!    (options.c:2400-2411), so `finish_transfer()` runs even for `recv_ok == 0`
//!    (receiver.c:1029) and the appended bytes stay on disk.
//! 4. **Warn and request the redo** (receiver.c:1063-1097).
//! 5. **Redo as an ordinary delta.** The generator re-enters `recv_generator()`
//!    with `append_mode` negated and `ignore_times` bumped
//!    (generator.c:2186-2200), and `whole_file` was already forced to 0 for the
//!    whole session because append mode is active (generator.c:2288-2289), so
//!    the retained partial is described as the delta basis
//!    (generator.c:1967 -> generate_and_send_sums).

use std::fs;
use std::path::{Path, PathBuf};

use ::metadata::MetadataOptions;

use crate::local_copy::{CopyContext, LocalCopyError, LocalCopyExecution};

use super::execute::execute_transfer_once;
use super::{TransferFlags, TransferOutcome};

/// Executes the data transfer for a single regular file, including the
/// `--append-verify` phase-2 redo when the first pass fails verification.
///
/// The common case is a single call to [`execute_transfer_once`]. Only an
/// `--append-verify` pass whose whole-file re-checksum disagreed runs a second
/// pass, and that pass is an ordinary delta transfer against the partial the
/// first pass left on disk.
#[allow(clippy::too_many_arguments)]
pub(in crate::local_copy) fn execute_transfer(
    context: &mut CopyContext,
    source: &Path,
    destination: &Path,
    metadata: &fs::Metadata,
    metadata_options: MetadataOptions,
    record_path: &Path,
    existing_metadata: Option<&fs::Metadata>,
    destination_previously_existed: bool,
    file_type: fs::FileType,
    relative: Option<&Path>,
    flags: TransferFlags,
    mode: LocalCopyExecution,
    copy_source_override: Option<PathBuf>,
    reference_basis: Option<PathBuf>,
) -> Result<(), LocalCopyError> {
    let outcome = execute_transfer_once(
        context,
        source,
        destination,
        metadata,
        metadata_options.clone(),
        record_path,
        existing_metadata,
        destination_previously_existed,
        file_type,
        relative,
        flags,
        mode,
        copy_source_override.clone(),
        reference_basis.clone(),
    )?;

    if outcome == TransferOutcome::Complete {
        return Ok(());
    }

    warn_verification_failed(context, record_path);

    // The first pass appended in place, so the destination now holds the full
    // source length with a wrong prefix. Re-stat it: that partial is the delta
    // basis for the redo, exactly as upstream's `generate_and_send_sums()` reads
    // the retained file. Losing it between the passes means there is nothing to
    // re-delta, so a stat failure is a hard error rather than a fall-back to a
    // whole-file copy.
    let retained = fs::symlink_metadata(destination).map_err(|error| {
        LocalCopyError::io("inspect retained partial", destination.to_path_buf(), error)
    })?;

    execute_transfer_once(
        context,
        source,
        destination,
        metadata,
        metadata_options,
        record_path,
        Some(&retained),
        // The destination existed before this pass: upstream counts
        // `stats.created_files++` only on the non-redo leg (receiver.c:778).
        true,
        file_type,
        relative,
        redo_flags(flags, context.sparse_enabled()),
        mode,
        copy_source_override,
        reference_basis,
    )?;

    Ok(())
}

/// The flag set upstream's generator installs around the phase-2 redo.
///
/// upstream: generator.c:2186-2188 negates `append_mode` and bumps
/// `ignore_times`; generator.c:2288-2289 already forced `whole_file = 0` for the
/// session because append mode is active, which is what overrides the
/// `whole_file = 1` default a local transfer would otherwise carry
/// (main.c:652-653); receiver.c:761,771 negates `sparse_files` alongside
/// `append_mode`, restoring whatever `--sparse` asked for.
fn redo_flags(flags: TransferFlags, sparse_enabled: bool) -> TransferFlags {
    TransferFlags {
        append_allowed: false,
        append_verify: false,
        whole_file_enabled: false,
        // Without this the quick-check would skip the redo outright: the
        // retained partial now has the source's size, and pass one already
        // stamped it with the source's mtime.
        ignore_times_enabled: true,
        use_sparse_writes: sparse_enabled,
        ..flags
    }
}

/// Emits upstream's retained-update warning for a failed verification.
///
/// upstream: receiver.c:1071-1094. The line is gated behind
/// `INFO_GTE(NAME, 1) || stdout_format_has_i` (receiver.c:1072) and carries the
/// transfer-relative name, never an absolute path.
///
/// `keptstr` is unconditionally `"retained"`. Upstream reaches that string
/// because `--append` implies `--inplace` (options.c:2400-2411), which takes
/// receiver.c:1073-1078 past both the `"discarded"` and the
/// `"put into partial-dir"` leg. The local executor arrives at the same place by
/// a different route: this warning is reachable only when `append_offset > 0`,
/// which pins `select_write_strategy` to `WriteStrategy::Append`, and that
/// strategy writes straight into the destination - so the update is retained
/// there no matter what `--partial-dir` says.
///
/// `redostr` is `" (will try again)"` because the local executor never replays a
/// batch, and the code is `FWARNING` rather than `FERROR_XFER` because the redo
/// this warning announces has not run yet (upstream's `redoing` is still 0).
///
/// The line goes to stderr because upstream emits it as `FWARNING`
/// (rsync.h:278, routed by log.c:314-316), matching the other stderr notices
/// this executor writes.
fn warn_verification_failed(context: &CopyContext, record_path: &Path) {
    if !logging::info_gte(logging::InfoFlag::Name, 1) && !context.options().is_itemize_active() {
        return;
    }
    eprintln!(
        "WARNING: {} failed verification -- update retained (will try again).",
        record_path.display()
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flags() -> TransferFlags {
        TransferFlags {
            append_allowed: true,
            append_verify: true,
            whole_file_enabled: true,
            inplace_enabled: false,
            partial_enabled: false,
            use_sparse_writes: false,
            compress_enabled: false,
            size_only_enabled: false,
            ignore_times_enabled: false,
            checksum_enabled: false,
            #[cfg(all(any(unix, windows), feature = "xattr"))]
            preserve_xattrs: false,
            xattrs_changed: false,
            #[cfg(all(any(unix, windows), feature = "acl"))]
            preserve_acls: false,
        }
    }

    #[test]
    fn redo_negates_append_and_forces_a_delta_pass() {
        // Each of these is load-bearing for the redo to reproduce upstream's
        // second pass: still appending would re-append nothing, whole-file
        // would re-send the file as pure literal instead of re-deltaing the
        // retained partial, and honouring times would skip the file entirely
        // because pass one already gave it the source's size and mtime.
        let redo = redo_flags(flags(), false);
        assert!(!redo.append_allowed);
        assert!(!redo.append_verify);
        assert!(!redo.whole_file_enabled);
        assert!(redo.ignore_times_enabled);
    }

    #[test]
    fn redo_restores_sparse_writes_from_the_session_setting() {
        // upstream: receiver.c:761,771 negates sparse_files alongside
        // append_mode, so --sparse suppressed during the append comes back for
        // the redo. Without --sparse it must stay off.
        assert!(redo_flags(flags(), true).use_sparse_writes);
        assert!(!redo_flags(flags(), false).use_sparse_writes);
    }

    #[test]
    fn redo_preserves_unrelated_flags() {
        // The redo negates only what upstream negates; everything else is the
        // session's setting and must survive.
        let mut base = flags();
        base.compress_enabled = true;
        base.checksum_enabled = true;
        base.inplace_enabled = true;
        let redo = redo_flags(base, false);
        assert!(redo.compress_enabled);
        assert!(redo.checksum_enabled);
        assert!(redo.inplace_enabled);
    }
}

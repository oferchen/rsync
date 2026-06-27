use std::io::{self, Write};
use std::path::Path;

use core::client::{
    ClientEntryKind, ClientEntryMetadata, ClientEvent, ClientEventKind, ClientSummary,
    HumanReadableMode,
};

/// Renders a path string with any trailing platform path separators trimmed,
/// mirroring upstream rsync's `*cp = '\0'` slash-lopping in `main.c:789`
/// before the `created directory %s\n` print.
fn display_without_trailing_separators(path: &Path) -> String {
    let mut rendered = path.display().to_string();
    while rendered.len() > 1
        && rendered
            .as_bytes()
            .last()
            .is_some_and(|&b| b == b'/' || (cfg!(windows) && b == b'\\'))
    {
        rendered.pop();
    }
    rendered
}

use super::format::{
    LIST_SIZE_WIDTH, event_matches_name_level, format_decimal_bytes, format_list_permissions,
    format_list_size, format_list_timestamp, format_progress_bytes, format_progress_elapsed,
    format_progress_percent, format_progress_rate, format_size, format_stat_categories,
    format_summary_rate, is_progress_event, list_only_event,
};
use super::mode::{NameOutputLevel, ProgressMode};
use crate::{OutFormat, OutFormatContext, emit_out_format};

#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_transfer_summary(
    summary: &ClientSummary,
    verbosity: u8,
    progress_mode: Option<ProgressMode>,
    stats_level: u8,
    progress_already_rendered: bool,
    list_only: bool,
    dry_run: bool,
    out_format: Option<&OutFormat>,
    out_format_context: &OutFormatContext,
    name_level: NameOutputLevel,
    name_overridden: bool,
    human_readable_mode: HumanReadableMode,
    suppress_updated_only_totals: bool,
    emit_flist_banner: bool,
    show_copy_method: bool,
    writer: &mut dyn Write,
) -> io::Result<()> {
    let events = summary.events();
    let stats_on = stats_level > 0;

    if list_only {
        let mut wrote_listing = false;
        if !events.is_empty() {
            emit_list_only(events, writer, human_readable_mode)?;
            wrote_listing = true;
        }

        if stats_on {
            if wrote_listing {
                writeln!(writer)?;
            }
            emit_stats(
                summary,
                writer,
                human_readable_mode,
                dry_run,
                stats_level,
                show_copy_method,
            )?;
        } else if verbosity > 0 {
            if wrote_listing {
                writeln!(writer)?;
            }
            emit_totals(
                summary,
                writer,
                human_readable_mode,
                dry_run,
                show_copy_method,
            )?;
        }

        return Ok(());
    }

    // upstream: flist.c:2252 - rprintf(FCLIENT, "sending incremental file list\n")
    // is gated on inc_recurse && INFO_GTE(FLIST, 1) && !am_server. The banner
    // is FCLIENT-only - the parallel `rprintf(FLOG, "building file list\n")`
    // covers the log file (already emitted via the logging::info_log! pipeline
    // in the generator). Local-copy mode is treated as inc_recurse-equivalent
    // because the source enumeration is interleaved with per-file dispatch.
    if emit_flist_banner && verbosity > 0 {
        writeln!(writer, "sending incremental file list")?;
    }

    // upstream: main.c:787-808 - when the receiver pre-flight-mkdirs the
    // destination root because `file_total > 1 || trailing_slash`, it lops
    // off the trailing slash (`*cp = '\0'`) and prints `created directory
    // %s\n` gated on `INFO_GTE(NAME, 1) || stdout_format_has_i`. The print at
    // main.c:808 precedes the `dry_run++` at main.c:810, so a dry-run still
    // reports the directory it would create. Mirror the same gate plus
    // trailing-slash trim here so `-i` and `-v` invocations - including
    // `--dry-run` - emit the notice ahead of the per-entry itemize lines,
    // matching the upstream `testsuite/itemize.test` golden.
    if summary.destination_root_created()
        && (out_format.is_some() || verbosity > 0)
        && let Some(dest_root) = events.iter().map(ClientEvent::destination_root).next()
    {
        writeln!(
            writer,
            "created directory {}",
            display_without_trailing_separators(dest_root)
        )?;
    }

    let formatted_rendered = if let Some(format) = out_format {
        if events.is_empty() {
            false
        } else {
            emit_out_format(events, format, out_format_context, writer)?;
            true
        }
    } else {
        false
    };

    let progress_rendered = if progress_already_rendered {
        true
    } else if matches!(progress_mode, Some(ProgressMode::PerFile)) && !events.is_empty() {
        emit_progress(events, writer, human_readable_mode)?
    } else {
        false
    };

    let name_enabled = !matches!(name_level, NameOutputLevel::Disabled);
    // upstream: with --progress each name is printed exactly once, inline
    // before its progress line (progress.c / rprintf name1 path). When the
    // per-file progress block already rendered the names, suppress the verbose
    // name listing so `--progress` (info=name1, verbosity 0) does not re-print
    // the whole list - mirroring the `!progress_rendered` guard already applied
    // to the verbosity>0 branch above.
    let emit_verbose_listing = out_format.is_none()
        && !events.is_empty()
        && ((verbosity > 0
            && (!name_overridden || name_enabled)
            && (!progress_rendered || verbosity > 1))
            || (verbosity == 0 && name_enabled && !progress_rendered));

    if formatted_rendered && (emit_verbose_listing || stats_on || verbosity > 0) {
        writeln!(writer)?;
    }

    if progress_rendered && (emit_verbose_listing || stats_on || verbosity > 0) {
        writeln!(writer)?;
    }

    if emit_verbose_listing {
        emit_verbose(
            events,
            verbosity,
            name_level,
            name_overridden,
            human_readable_mode,
            writer,
        )?;
    }

    let name_enabled = !matches!(name_level, NameOutputLevel::Disabled);
    // upstream: main.c output_summary() emits the `sent/received/total size`
    // trailer only for `verbose > 0 || INFO_GTE(STATS, 1)`; itemize alone
    // (`-i` or `-ii`, no `-v`/`--stats`) never shows it. `suppress_updated_only_totals`
    // captures that itemize-without-verbose-or-stats condition, so it applies to
    // both itemize name levels (`UpdatedOnly` for `-i`, `UpdatedAndUnchanged` for `-ii`).
    let suppress_name_totals = suppress_updated_only_totals
        && matches!(
            name_level,
            NameOutputLevel::UpdatedOnly | NameOutputLevel::UpdatedAndUnchanged
        );
    let emit_trailer_totals =
        !stats_on && (verbosity > 0 || (name_enabled && !suppress_name_totals));

    // upstream: main.c:461 - `output_summary()` emits a blank
    // `rprintf(FCLIENT, "\n")` before the INFO_GTE(STATS, 1) totals so the
    // trailer is visually separated from preceding per-file output.
    // `testsuite/itemize.test`'s `v_filt` helper relies on this empty line
    // (`sed -e '/^$/,$d'`) to strip the trailer when matching `-vv` goldens.
    if emit_verbose_listing && (stats_on || emit_trailer_totals) {
        writeln!(writer)?;
    }

    if stats_on {
        emit_stats(
            summary,
            writer,
            human_readable_mode,
            dry_run,
            stats_level,
            show_copy_method,
        )?;
    } else if emit_trailer_totals {
        emit_totals(
            summary,
            writer,
            human_readable_mode,
            dry_run,
            show_copy_method,
        )?;
    }

    Ok(())
}

pub(crate) fn emit_list_only<W: Write + ?Sized>(
    events: &[ClientEvent],
    stdout: &mut W,
    human_readable: HumanReadableMode,
) -> io::Result<()> {
    for event in events {
        if !list_only_event(event.kind()) {
            continue;
        }

        if let Some(metadata) = event.metadata() {
            let permissions = format_list_permissions(metadata);
            let size = format_list_size(metadata.length(), human_readable);
            let timestamp = format_list_timestamp(metadata.modified());
            let mut rendered = event.relative_path().to_string_lossy().into_owned();
            if metadata.kind().is_symlink()
                && let Some(target) = metadata.symlink_target()
            {
                rendered.push_str(" -> ");
                rendered.push_str(&target.to_string_lossy());
            }

            writeln!(stdout, "{permissions} {size} {timestamp} {rendered}")?;
        } else {
            let rendered = event.relative_path().to_string_lossy().into_owned();
            writeln!(
                stdout,
                "?????????? {:>width$} {} {rendered}",
                "?",
                format_list_timestamp(None),
                width = LIST_SIZE_WIDTH,
            )?;
        }
    }

    Ok(())
}

/// Renders progress lines for the provided transfer events.
pub(crate) fn emit_progress<W: Write + ?Sized>(
    events: &[ClientEvent],
    stdout: &mut W,
    human_readable: HumanReadableMode,
) -> io::Result<bool> {
    // upstream: progress.c counts every checked file-list entry toward
    // `to-chk=<remaining>/<total>`, but prints a per-file block and advances
    // `xfr#` only for entries actually transferred. An up-to-date match
    // (quick-check) is silent under `--progress`/`-P` (it surfaces only with
    // `-vv`/`-i`), so a no-change run prints no per-file lines.
    let flist_entries: Vec<_> = events
        .iter()
        .filter(|event| is_progress_event(event.kind()))
        .collect();

    // Denominator counts every checked entry; the numerator counts down only the
    // transfers, so to-chk reaches 0 on the last transferred file even when an
    // up-to-date entry (e.g. an unchanged parent dir) trails it in the list.
    let total = flist_entries.len();
    let transferred_total = flist_entries
        .iter()
        .filter(|event| !is_uptodate_event(event))
        .count();
    if transferred_total == 0 {
        return Ok(false);
    }

    let mut xfr_index = 0usize;
    for event in flist_entries.into_iter() {
        if is_uptodate_event(event) {
            continue;
        }
        xfr_index += 1;
        let remaining = transferred_total - xfr_index;

        writeln!(stdout, "{}", event.relative_path().display())?;

        let bytes = event.bytes_transferred();
        // Field widths mirror upstream rsync's `rprint_progress` format string
        // `"\r%15s %3d%% %7.2f%s %s%s"` (progress.c:129). The rate column packs the
        // `%7.2f` value (7 chars) plus a 4-char unit suffix (kB/s, MB/s, GB/s) for an
        // 11-char total. The elapsed column matches `%4u:%02u:%02u` at 10 chars.
        let size_field = format!("{:>15}", format_progress_bytes(bytes, human_readable));
        let percent_hint = match event.kind() {
            ClientEventKind::DataCopied => event.total_bytes(),
            _ => None,
        };
        let percent_field = format!("{:>4}", format_progress_percent(bytes, percent_hint));
        let rate_field = format!(
            "{:>11}",
            format_progress_rate(bytes, event.elapsed(), human_readable)
        );
        let elapsed_field = format!("{:>10}", format_progress_elapsed(event.elapsed()));

        writeln!(
            stdout,
            "{size_field} {percent_field} {rate_field} {elapsed_field} (xfr#{xfr_index}, to-chk={remaining}/{total})"
        )?;
    }

    Ok(true)
}

/// Emits a statistics summary mirroring the subset of counters supported by the local engine.
///
/// Output is gated by `level`, matching upstream rsync's `INFO_GTE(STATS, N)`
/// checks in `output_summary` (`main.c:416-465`):
///
/// - level 0: emits nothing.
/// - level 1: emits only the trailing `sent X / total size is Y` summary.
/// - level 2+: emits the full file-count + byte-breakdown block followed by
///   the level-1 summary. The `File list generation/transfer time` lines are
///   only emitted when the corresponding counter is non-zero, matching
///   upstream's `if (stats.flist_buildtime)` guard.
///
/// Line ordering and label spelling track upstream byte-for-byte. The leading
/// `\n` (`main.c:419`) is intentionally omitted; the caller is responsible
/// for the blank-line separator between prior output blocks and stats.
///
/// upstream: main.c output_summary (rsync-3.4.2 lines 416-465)
/// upstream: main.c handle_stats (rsync-3.4.2 lines 325-385)
pub(crate) fn emit_stats<W: Write + ?Sized>(
    summary: &ClientSummary,
    stdout: &mut W,
    human_readable: HumanReadableMode,
    dry_run: bool,
    level: u8,
    show_copy_method: bool,
) -> io::Result<()> {
    if level == 0 {
        return Ok(());
    }

    if level >= 2 {
        emit_stats_detail_block(summary, stdout, human_readable)?;
        writeln!(stdout)?;
    }

    emit_totals(summary, stdout, human_readable, dry_run, show_copy_method)
}

/// Emits the level-2+ detail block: file counts, byte-breakdown, file-list
/// timing, and total bytes sent/received.
///
/// upstream: main.c output_summary block under `INFO_GTE(STATS, 2)` (rsync-3.4.2:418-449)
fn emit_stats_detail_block<W: Write + ?Sized>(
    summary: &ClientSummary,
    stdout: &mut W,
    human_readable: HumanReadableMode,
) -> io::Result<()> {
    let files = summary.files_copied();
    let files_total = summary.regular_files_total();
    let directories = summary.directories_created();
    let directories_total = summary.directories_total();
    let symlinks = summary.symlinks_copied();
    let symlinks_total = summary.symlinks_total();
    let devices = summary.devices_created();
    let devices_total = summary.devices_total();
    let fifos = summary.fifos_created();
    let fifos_total = summary.fifos_total();
    let deleted = summary.items_deleted();
    let literal_bytes = summary.bytes_copied();
    let transferred_size = summary.transferred_file_size();
    let bytes_sent = summary.bytes_sent();
    let bytes_received = summary.bytes_received();
    let total_size = summary.total_source_bytes();
    let matched_bytes = summary.matched_bytes();
    let file_list_size = summary.file_list_size();
    let file_list_generation_ms = summary.file_list_generation_time().as_millis();
    let file_list_transfer_ms = summary.file_list_transfer_time().as_millis();
    let file_list_generation = summary.file_list_generation_time().as_secs_f64();
    let file_list_transfer = summary.file_list_transfer_time().as_secs_f64();

    let special_total = devices_total.saturating_add(fifos_total);
    let special_created = devices.saturating_add(fifos);
    let total_entries = files_total
        .saturating_add(directories_total)
        .saturating_add(symlinks_total)
        .saturating_add(special_total);
    let created_total = files
        .saturating_add(directories)
        .saturating_add(symlinks)
        .saturating_add(special_created);

    let files_breakdown = format_stat_categories(&[
        ("reg", files_total),
        ("dir", directories_total),
        ("link", symlinks_total),
        ("special", special_total),
    ]);
    let created_breakdown = format_stat_categories(&[
        ("reg", files),
        ("dir", directories),
        ("link", symlinks),
        ("special", special_created),
    ]);

    let total_size_display = format_size(total_size, human_readable);
    let transferred_size_display = format_size(transferred_size, human_readable);
    let literal_bytes_display = format_size(literal_bytes, human_readable);
    let matched_bytes_display = format_size(matched_bytes, human_readable);
    let file_list_size_display = format_size(file_list_size, human_readable);
    let bytes_sent_display = format_size(bytes_sent, human_readable);
    let bytes_received_display = format_size(bytes_received, human_readable);

    // upstream: main.c output_summary() - every count is wrapped in
    // comma_num(), e.g. `rprintf(FINFO, "Number of files: %s%s\n",
    // comma_num(stats.num_files), ...)`. comma_num inserts thousands
    // separators unconditionally (independent of -h), so a count >= 1000
    // renders as `1,500`, not `1500`.
    writeln!(
        stdout,
        "Number of files: {}{files_breakdown}",
        format_decimal_bytes(total_entries)
    )?;
    writeln!(
        stdout,
        "Number of created files: {}{created_breakdown}",
        format_decimal_bytes(created_total)
    )?;
    writeln!(
        stdout,
        "Number of deleted files: {}",
        format_decimal_bytes(deleted)
    )?;
    writeln!(
        stdout,
        "Number of regular files transferred: {}",
        format_decimal_bytes(files)
    )?;
    writeln!(stdout, "Total file size: {total_size_display} bytes")?;
    writeln!(
        stdout,
        "Total transferred file size: {transferred_size_display} bytes"
    )?;
    writeln!(stdout, "Literal data: {literal_bytes_display} bytes")?;
    writeln!(stdout, "Matched data: {matched_bytes_display} bytes")?;
    writeln!(stdout, "File list size: {file_list_size_display}")?;
    // upstream: main.c:437 `if (stats.flist_buildtime)` gates both timing
    // lines. The upstream counter is a millisecond integer, so sub-millisecond
    // durations suppress the lines just as on the C side.
    if file_list_generation_ms > 0 || file_list_transfer_ms > 0 {
        writeln!(
            stdout,
            "File list generation time: {file_list_generation:.3} seconds"
        )?;
        writeln!(
            stdout,
            "File list transfer time: {file_list_transfer:.3} seconds"
        )?;
    }
    writeln!(stdout, "Total bytes sent: {bytes_sent_display}")?;
    writeln!(stdout, "Total bytes received: {bytes_received_display}")?;
    Ok(())
}

/// Emits the summary lines reported by verbose transfers.
pub(crate) fn emit_totals<W: Write + ?Sized>(
    summary: &ClientSummary,
    stdout: &mut W,
    human_readable: HumanReadableMode,
    dry_run: bool,
    show_copy_method: bool,
) -> io::Result<()> {
    let sent = summary.bytes_sent();
    let received = summary.bytes_received();
    let total_size = summary.total_source_bytes();
    // upstream main.c:418-423: rate = (written+read) / (0.5 + (endtime-starttime)),
    // a single wall-clock span with a 0.5s floor - never the summed per-file copy
    // durations (which are ~0 for CoW/clonefile and explode the rate).
    let wall_seconds = summary.wall_clock_elapsed().as_secs_f64();
    let rate = (sent + received) as f64 / (0.5 + wall_seconds);
    let transmitted = sent.saturating_add(received);
    let speedup = if transmitted > 0 {
        total_size as f64 / transmitted as f64
    } else {
        0.0
    };

    let sent_display = format_size(sent, human_readable);
    let received_display = format_size(received, human_readable);
    let rate_display = format_summary_rate(rate, human_readable);
    let total_size_display = format_size(total_size, human_readable);

    // oc-rsync extension: when a local copy used a kernel acceleration
    // (clonefile/reflink/io_uring), report which technology moved the data so
    // the byte totals make sense (a CoW clone copies no bytes). Opt-in via
    // `--info=copy`, and further gated on an accelerated method actually running
    // so it never alters default or protocol-transfer output (upstream parity).
    if show_copy_method && summary.used_copy_acceleration() {
        let methods = summary
            .copy_method_breakdown()
            .into_iter()
            .map(|(label, count)| format!("{label} x{count}"))
            .collect::<Vec<_>>()
            .join(", ");
        writeln!(stdout, "Copy method: {methods}")?;
    }

    writeln!(
        stdout,
        "sent {sent_display} bytes  received {received_display} bytes  {rate_display} bytes/sec"
    )?;
    let dry_run_suffix = if dry_run { " (DRY RUN)" } else { "" };
    // upstream: main.c:466-468 - speedup uses comma_dnum(_, 2), i.e. thousands
    // grouping. Reuse the same helper the --stats path uses (stats_format.rs)
    // so both summary paths group identically.
    let speedup_display = crate::stats_format::format_speedup(speedup);
    writeln!(
        stdout,
        "total size is {total_size_display}  speedup is {speedup_display}{dry_run_suffix}"
    )
}

/// Returns whether an event represents a no-op uptodate emission. Used to
/// reorder events so upstream's generator-first / receiver-second wire order
/// is preserved in verbose mode.
///
/// upstream: hlink.c:218-224, generator.c:1010-1022, rsync.c:672-676 - the
/// generator emits `"is uptodate"` synchronously while the receiver emits
/// the bare-name notice from `set_file_attrs` only after the transfer
/// completes. The two processes pipeline so uptodate lines appear ahead of
/// transferred-file lines in the observable client output.
fn is_uptodate_event(event: &ClientEvent) -> bool {
    event.is_uptodate()
}

/// Returns `true` for a HardLink event describing a hard-linked symlink (`hL`).
/// Its metadata kind is `Symlink` and its `symlink_target` slot holds the link
/// target, not a `=> leader` trailer.
fn is_hardlinked_symlink_event(event: &ClientEvent) -> bool {
    matches!(event.kind(), ClientEventKind::HardLink)
        && event
            .metadata()
            .map(ClientEntryMetadata::kind)
            .is_some_and(|kind| matches!(kind, ClientEntryKind::Symlink))
}

/// Returns `true` for a hardlink alias freshly linked to a leader placed during
/// this run. Upstream defers the `"%s => %s"` notice to the hardlink-finishing
/// phase (hlink.c:236), so it appears after the regular per-file lines. A
/// hard-linked symlink is excluded (its trailer is `-> target`, not `=> leader`).
fn is_deferred_hardlink_event(event: &ClientEvent) -> bool {
    matches!(event.kind(), ClientEventKind::HardLink)
        && !event.is_hardlink_uptodate()
        && !is_hardlinked_symlink_event(event)
        && event
            .metadata()
            .and_then(ClientEntryMetadata::symlink_target)
            .is_some()
}

/// Renders an event's path for the verbose file listing, mirroring upstream
/// `f_name()` (flist.c): directory names carry a trailing `/` and the transfer
/// root (`.`) renders as `./`. Non-directory entries render verbatim. This
/// matches the receiver-side verbose path (receiver/transfer/sync.rs) so a
/// local-copy `-v`/`--info=name` listing agrees with upstream.
///
/// upstream: log.c:639-640 - the `%n` token strlcat()s a `/` for S_ISDIR
/// entries; the transfer root is listed as `./`.
fn verbose_listing_name(event: &ClientEvent) -> String {
    let path = event.relative_path();
    let is_dir = matches!(
        event.metadata().map(ClientEntryMetadata::kind),
        Some(ClientEntryKind::Directory)
    );
    if is_dir && path.as_os_str() == "." {
        return String::from("./");
    }
    let mut rendered = path.to_string_lossy().into_owned();
    // upstream: flist.c f_name() emits POSIX forward-slash separators
    // regardless of host OS. Normalize Windows native backslashes at the
    // rendering boundary; storage retains the platform-native form.
    #[cfg(windows)]
    {
        rendered = rendered.replace('\\', "/");
    }
    if is_dir {
        rendered.push('/');
    }
    rendered
}

/// Renders verbose listings for the provided transfer events.
pub(crate) fn emit_verbose<W: Write + ?Sized>(
    events: &[ClientEvent],
    verbosity: u8,
    name_level: NameOutputLevel,
    name_overridden: bool,
    _human_readable: HumanReadableMode,
    stdout: &mut W,
) -> io::Result<()> {
    if matches!(name_level, NameOutputLevel::Disabled) && (verbosity == 0 || name_overridden) {
        return Ok(());
    }

    // upstream pipelines the generator (which emits `"is uptodate"`
    // synchronously) ahead of the receiver (which emits the bare-name
    // notice only after `set_file_attrs` returns). In our event-stream
    // pipeline the actions are recorded in traversal order, so partition
    // them to recover the upstream wire order. The sort is stable so each
    // group preserves its original relative ordering.
    let mut ordered_events: Vec<&ClientEvent> = events.iter().collect();
    ordered_events.sort_by_key(|event| {
        if is_uptodate_event(event) {
            0u8
        } else if is_deferred_hardlink_event(event) {
            // Hardlink aliases linked to a this-run leader are finished last.
            2
        } else {
            1
        }
    });

    for event in ordered_events {
        let kind = event.kind();
        let include_for_name = event_matches_name_level(event, name_level);

        if verbosity == 0 {
            if !include_for_name {
                continue;
            }

            // upstream: rsync.c:676 - uptodate notice uses `"%s is uptodate"`
            // wording at INFO_GTE(NAME, 2). `--info=name2` sets name_level to
            // UpdatedAndUnchanged here, so route MetadataReused (or a
            // HardLink whose destination was already linked to the leader)
            // through the uptodate phrasing instead of the bare path.
            //
            // Skip directory MetadataReused events: upstream invokes
            // `set_file_attrs(..., 0)` for dirs (generator.c:1503), so the
            // rsync.c:676 "is uptodate" notice is gated off for them.
            if matches!(kind, ClientEventKind::MetadataReused) || event.is_hardlink_uptodate() {
                if event
                    .metadata()
                    .map(ClientEntryMetadata::kind)
                    .is_some_and(|kind| matches!(kind, ClientEntryKind::Directory))
                {
                    continue;
                }
                writeln!(stdout, "{} is uptodate", event.relative_path().display())?;
                continue;
            }

            let mut rendered = verbose_listing_name(event);
            if matches!(kind, ClientEventKind::SymlinkCopied)
                && let Some(metadata) = event.metadata()
                && let Some(target) = metadata.symlink_target()
            {
                rendered.push_str(" -> ");
                rendered.push_str(&target.to_string_lossy());
            }
            writeln!(stdout, "{rendered}")?;
            continue;
        }

        if name_overridden && !include_for_name {
            continue;
        }

        match kind {
            ClientEventKind::SkippedExisting => {
                // upstream: generator.c:1409 - `rprintf(FINFO, "%s exists\n",
                // fname)` for an --ignore-existing skip: the bare relative name
                // followed by " exists", no descriptor and no quotes.
                writeln!(stdout, "{} exists", event.relative_path().display())?;
                continue;
            }
            ClientEventKind::SkippedMissingDestination => {
                writeln!(
                    stdout,
                    "skipping non-existent destination file \"{}\"",
                    event.relative_path().display()
                )?;
                continue;
            }
            ClientEventKind::SkippedNewerDestination => {
                writeln!(
                    stdout,
                    "skipping newer destination file \"{}\"",
                    event.relative_path().display()
                )?;
                continue;
            }
            ClientEventKind::SkippedNonRegular => {
                writeln!(
                    stdout,
                    "skipping non-regular file \"{}\"",
                    event.relative_path().display()
                )?;
                continue;
            }
            ClientEventKind::SkippedDirectory => {
                writeln!(
                    stdout,
                    "skipping directory \"{}\" (no recursion)",
                    event.relative_path().display()
                )?;
                continue;
            }
            ClientEventKind::SkippedUnsafeSymlink => {
                let mut rendered = format!(
                    "ignoring unsafe symlink \"{}\"",
                    event.relative_path().display()
                );
                if let Some(metadata) = event.metadata()
                    && let Some(target) = metadata.symlink_target()
                {
                    rendered.push_str(" -> ");
                    rendered.push_str(&target.to_string_lossy());
                }
                writeln!(stdout, "{rendered}")?;
                continue;
            }
            ClientEventKind::SkippedMountPoint => {
                writeln!(
                    stdout,
                    "skipping mount point \"{}\"",
                    event.relative_path().display()
                )?;
                continue;
            }
            ClientEventKind::MetadataReused => {
                // upstream: rsync.c:672-676 - rprintf(FCLIENT, "%s is uptodate\n", fname)
                // is gated by INFO_GTE(NAME, 2). At plain -v (verbose=1) the
                // notice is suppressed and unchanged files leave no per-file
                // trace. Emit only when NAME>=2 - either via -vv or explicit
                // --info=name2 (which sets name_level to UpdatedAndUnchanged).
                // The local-copy engine intentionally does NOT emit this via
                // info_log! to avoid the diagnostic-flush ordering hazard
                // (info_log! events drain AFTER the summary in cli mod.rs);
                // routing it through the event renderer keeps `is uptodate`
                // lines ahead of the totals to match upstream's wire order.
                //
                // upstream: generator.c:1503 calls `set_file_attrs(..., 0)`
                // for directories (no ATTRS_REPORT flag), so dirs never trigger
                // the rsync.c:676 "is uptodate" notice. Symlinks (line 1575)
                // and regular files (line 1827) DO pass `maybe_ATTRS_REPORT`
                // and therefore surface. Skip directory MetadataReused events
                // here so the `-vv` golden in `testsuite/itemize.test` matches.
                if event
                    .metadata()
                    .map(ClientEntryMetadata::kind)
                    .is_some_and(|kind| matches!(kind, ClientEntryKind::Directory))
                {
                    continue;
                }
                if verbosity >= 2 || matches!(name_level, NameOutputLevel::UpdatedAndUnchanged) {
                    writeln!(stdout, "{} is uptodate", event.relative_path().display())?;
                }
                continue;
            }
            ClientEventKind::HardLink if event.is_hardlink_uptodate() => {
                // upstream: hlink.c:218-224 - when the destination already
                // shares the source group leader's inode, the generator
                // emits `"%s is uptodate"` at INFO_GTE(NAME, 2) instead of
                // the bare path. Mirror the same gate so `-vv` without `-i`
                // matches the upstream `testsuite/itemize.test` golden.
                if verbosity >= 2 || matches!(name_level, NameOutputLevel::UpdatedAndUnchanged) {
                    writeln!(stdout, "{} is uptodate", event.relative_path().display())?;
                }
                continue;
            }
            // upstream: generator.c:1021-1022 / 1044-1046 / 1145-1147 - a
            // `--copy-dest` match that needs no transfer prints `"%s%s is
            // uptodate\n"` at INFO_GTE(NAME, 2), with a trailing `/` for
            // directories, and prints nothing at lower verbosity (the bare
            // per-file name is only emitted for entries that were actually
            // transferred). Regular files are recorded as `ReferenceCopied`;
            // directories and symlinks reconstructed from the basis carry a
            // blank change set and no creation flag. Genuine new entries keep
            // `was_created`, so they fall through to the bare-path emission.
            ClientEventKind::ReferenceCopied => {
                if verbosity >= 2 || matches!(name_level, NameOutputLevel::UpdatedAndUnchanged) {
                    writeln!(stdout, "{} is uptodate", event.relative_path().display())?;
                }
                continue;
            }
            // upstream: generator.c:1145-1147 - a `--link-dest` symlink
            // hard-linked from the basis prints `"%s is uptodate"` at NAME>=2.
            ClientEventKind::HardLink if is_hardlinked_symlink_event(event) => {
                if verbosity >= 2 || matches!(name_level, NameOutputLevel::UpdatedAndUnchanged) {
                    writeln!(stdout, "{} is uptodate", event.relative_path().display())?;
                }
                continue;
            }
            ClientEventKind::DirectoryCreated
                if !event.was_created() && !event.change_set().has_any_change() =>
            {
                if verbosity >= 2 || matches!(name_level, NameOutputLevel::UpdatedAndUnchanged) {
                    writeln!(stdout, "{}/ is uptodate", event.relative_path().display())?;
                }
                continue;
            }
            ClientEventKind::SymlinkCopied
                if !event.was_created() && !event.change_set().has_any_change() =>
            {
                if verbosity >= 2 || matches!(name_level, NameOutputLevel::UpdatedAndUnchanged) {
                    writeln!(stdout, "{} is uptodate", event.relative_path().display())?;
                }
                continue;
            }
            _ => {}
        }

        let mut rendered = verbose_listing_name(event);
        if matches!(kind, ClientEventKind::SymlinkCopied)
            && let Some(metadata) = event.metadata()
            && let Some(target) = metadata.symlink_target()
        {
            rendered.push_str(" -> ");
            rendered.push_str(&target.to_string_lossy());
        } else if matches!(kind, ClientEventKind::HardLink)
            && let Some(metadata) = event.metadata()
            && let Some(leader) = metadata.symlink_target()
        {
            // upstream: hlink.c:236 - a freshly-linked alias prints
            // `"%s => %s"` with the group leader's relative path.
            rendered.push_str(" => ");
            rendered.push_str(&leader.to_string_lossy());
        }

        // upstream: log.c:log_formatted() emits the default `%n%L` per-file
        // line at every verbosity tier (set in options.c:2372). The rendered
        // string already includes the `-> target` suffix for symlinks; higher
        // tiers only add ancillary log messages, never a per-file descriptor
        // prefix or byte-count wrapper.
        writeln!(stdout, "{rendered}")?;
    }
    Ok(())
}

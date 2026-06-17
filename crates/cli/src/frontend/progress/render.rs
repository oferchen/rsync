use std::io::{self, Write};
use std::path::Path;

use core::client::{ClientEvent, ClientEventKind, ClientSummary, HumanReadableMode};

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
    event_matches_name_level, format_list_permissions, format_list_size, format_list_timestamp,
    format_progress_bytes, format_progress_elapsed, format_progress_percent, format_progress_rate,
    format_size, format_stat_categories, format_summary_rate, is_progress_event, list_only_event,
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
            emit_stats(summary, writer, human_readable_mode, dry_run, stats_level)?;
        } else if verbosity > 0 {
            if wrote_listing {
                writeln!(writer)?;
            }
            emit_totals(summary, writer, human_readable_mode, dry_run)?;
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
    // %s\n` gated on `INFO_GTE(NAME, 1) || stdout_format_has_i`. Mirror the
    // same gate plus trailing-slash trim here so `-i` and `-v` invocations
    // emit the notice ahead of the per-entry itemize lines, matching the
    // upstream `testsuite/itemize.test` golden. The notice is only emitted
    // when the local-copy executor reports that it materialised the
    // destination root during this run.
    if summary.destination_root_created()
        && !dry_run
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
    let emit_verbose_listing = out_format.is_none()
        && !events.is_empty()
        && ((verbosity > 0
            && (!name_overridden || name_enabled)
            && (!progress_rendered || verbosity > 1))
            || (verbosity == 0 && name_enabled));

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
        if stats_on {
            writeln!(writer)?;
        }
    }

    let name_enabled = !matches!(name_level, NameOutputLevel::Disabled);
    let suppress_name_totals =
        suppress_updated_only_totals && matches!(name_level, NameOutputLevel::UpdatedOnly);

    if stats_on {
        emit_stats(summary, writer, human_readable_mode, dry_run, stats_level)?;
    } else if verbosity > 0 || (name_enabled && !suppress_name_totals) {
        emit_totals(summary, writer, human_readable_mode, dry_run)?;
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
                "?????????? {:>15} {} {rendered}",
                "?",
                format_list_timestamp(None),
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
    let progress_events: Vec<_> = events
        .iter()
        .filter(|event| is_progress_event(event.kind()))
        .collect();

    if progress_events.is_empty() {
        return Ok(false);
    }

    let total = progress_events.len();

    for (index, event) in progress_events.into_iter().enumerate() {
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
        let remaining = total - index - 1;
        let xfr_index = index + 1;

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
) -> io::Result<()> {
    if level == 0 {
        return Ok(());
    }

    if level >= 2 {
        emit_stats_detail_block(summary, stdout, human_readable)?;
        writeln!(stdout)?;
    }

    emit_totals(summary, stdout, human_readable, dry_run)
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

    writeln!(stdout, "Number of files: {total_entries}{files_breakdown}")?;
    writeln!(
        stdout,
        "Number of created files: {created_total}{created_breakdown}"
    )?;
    writeln!(stdout, "Number of deleted files: {deleted}")?;
    writeln!(stdout, "Number of regular files transferred: {files}")?;
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
) -> io::Result<()> {
    let sent = summary.bytes_sent();
    let received = summary.bytes_received();
    let total_size = summary.total_source_bytes();
    let elapsed = summary.total_elapsed();
    let seconds = elapsed.as_secs_f64();
    let rate = if seconds > 0.0 {
        (sent + received) as f64 / seconds
    } else {
        0.0
    };
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

    writeln!(
        stdout,
        "sent {sent_display} bytes  received {received_display} bytes  {rate_display} bytes/sec"
    )?;
    let dry_run_suffix = if dry_run { " (DRY RUN)" } else { "" };
    writeln!(
        stdout,
        "total size is {total_size_display}  speedup is {speedup:.2}{dry_run_suffix}"
    )
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

    for event in events {
        let kind = event.kind();
        let include_for_name = event_matches_name_level(event, name_level);

        if verbosity == 0 {
            if !include_for_name {
                continue;
            }

            // upstream: rsync.c:676 - uptodate notice uses `"%s is uptodate"`
            // wording at INFO_GTE(NAME, 2). `--info=name2` sets name_level to
            // UpdatedAndUnchanged here, so route MetadataReused through the
            // uptodate phrasing instead of the bare path.
            if matches!(kind, ClientEventKind::MetadataReused) {
                writeln!(stdout, "{} is uptodate", event.relative_path().display())?;
                continue;
            }

            let mut rendered = event.relative_path().to_string_lossy().into_owned();
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
                writeln!(
                    stdout,
                    "skipping existing file \"{}\"",
                    event.relative_path().display()
                )?;
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
                if verbosity >= 2 || matches!(name_level, NameOutputLevel::UpdatedAndUnchanged) {
                    writeln!(stdout, "{} is uptodate", event.relative_path().display())?;
                }
                continue;
            }
            _ => {}
        }

        let mut rendered = event.relative_path().to_string_lossy().into_owned();
        if matches!(kind, ClientEventKind::SymlinkCopied)
            && let Some(metadata) = event.metadata()
            && let Some(target) = metadata.symlink_target()
        {
            rendered.push_str(" -> ");
            rendered.push_str(&target.to_string_lossy());
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

use std::io::{self, Write};
use std::path::Path;

use core::client::{
    ClientEntryKind, ClientEntryMetadata, ClientEvent, ClientEventKind, ClientSummary,
    HumanReadableMode,
};

use crate::frontend::escape::escape_path;

/// Writes `<prefix><escaped path><suffix>\n` to a byte sink.
///
/// The escaped path is written as raw bytes so a lone invalid-UTF-8 byte under
/// `--8-bit-output` reaches the sink unmodified, matching upstream
/// `filtered_fwrite`; interpolating it through a `String` would replace it with
/// U+FFFD.
fn writeln_wrapped<W: Write + ?Sized>(
    stdout: &mut W,
    prefix: &str,
    path: &Path,
    allow_8bit: bool,
    suffix: &str,
) -> io::Result<()> {
    stdout.write_all(prefix.as_bytes())?;
    stdout.write_all(&escape_path(path, allow_8bit))?;
    stdout.write_all(suffix.as_bytes())?;
    stdout.write_all(b"\n")
}

/// Renders a path with any trailing platform path separators trimmed as raw
/// bytes, mirroring upstream rsync's `*cp = '\0'` slash-lopping in `main.c:789`
/// before the `created directory %s\n` print.
fn display_without_trailing_separators(path: &Path, allow_8bit: bool) -> Vec<u8> {
    let mut rendered = escape_path(path, allow_8bit);
    while rendered.len() > 1
        && rendered
            .last()
            .is_some_and(|&b| b == b'/' || (cfg!(windows) && b == b'\\'))
    {
        rendered.pop();
    }
    rendered
}

use super::format::{
    event_matches_name_level, format_count, format_list_permissions, format_list_size,
    format_list_timestamp, format_progress_bytes, format_progress_elapsed, format_progress_percent,
    format_progress_rate, format_size, format_stat_categories, format_summary_rate,
    is_progress_event, list_only_event,
};
use super::mode::{NameOutputLevel, ProgressMode};
use crate::{OutFormat, OutFormatContext, emit_out_format};
use logging::{InfoFlag, info_gte};

#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_transfer_summary(
    summary: &ClientSummary,
    verbosity: u8,
    progress_mode: Option<ProgressMode>,
    stats_level: u8,
    progress_already_rendered: bool,
    list_only: bool,
    dry_run: bool,
    // `--only-write-batch` (upstream `write_batch < 0`): appends the
    // `" (BATCH ONLY)"` speedup suffix, taking precedence over `--dry-run`.
    only_write_batch: bool,
    out_format: Option<&OutFormat>,
    out_format_context: &OutFormatContext,
    name_level: NameOutputLevel,
    name_overridden: bool,
    human_readable_mode: HumanReadableMode,
    suppress_updated_only_totals: bool,
    emit_flist_banner: bool,
    show_copy_method: bool,
    show_atimes: bool,
    show_crtimes: bool,
    eight_bit_output: bool,
    writer: &mut dyn Write,
) -> io::Result<()> {
    let events = summary.events();
    let stats_on = stats_level > 0;

    if list_only {
        let mut wrote_listing = false;
        if !events.is_empty() {
            emit_list_only(
                events,
                writer,
                human_readable_mode,
                show_atimes,
                show_crtimes,
                eight_bit_output,
                out_format_context.preserve_links(),
            )?;
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
                only_write_batch,
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
                only_write_batch,
                show_copy_method,
            )?;
        }

        return Ok(());
    }

    // upstream: flist.c:2251 - rprintf(FCLIENT, "sending incremental file list\n")
    // is gated on inc_recurse && INFO_GTE(FLIST, 1) && !am_server. This banner is
    // stdout-only. Upstream's parallel `rprintf(FLOG, "building file list\n")`
    // (flist.c:2248) targets the log file, which a plain client without
    // --log-file discards, so it never reaches stdout. Local-copy mode is treated
    // as inc_recurse-equivalent because the source enumeration is interleaved with
    // per-file dispatch.
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
    // matching the upstream `testsuite/itemize.test` golden. Gate on the NAME
    // info category (`INFO_GTE(NAME, 1)`) rather than a raw `verbosity > 0`
    // check so `--info=name0` suppresses the notice, while `-i`/`--out-format`
    // still forces it via `stdout_format_has_i` (mirrored by `out_format`).
    if summary.destination_root_created()
        && (out_format.is_some() || info_gte(InfoFlag::Name, 1))
        && let Some(dest_root) = events.iter().map(ClientEvent::destination_root).next()
    {
        writer.write_all(b"created directory ")?;
        writer.write_all(&display_without_trailing_separators(
            dest_root,
            eight_bit_output,
        ))?;
        writer.write_all(b"\n")?;
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
        emit_progress(events, writer, human_readable_mode, eight_bit_output)?
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
            eight_bit_output,
            writer,
        )?;
    }

    // upstream: main.c:459-461 output_summary() emits the
    // `sent/received/total size` trailer only when
    // `verbose > 0 || INFO_GTE(STATS, 1)`. `stats_on` already captures the
    // STATS>=1 arm (it routes to emit_stats below), so the name-only and
    // itemize cases - `--info=name1`/`--info=name2`, `--progress`, `-i`/`-ii`
    // (verbose 0, no stats level) - correctly print no trailer. A plain
    // `-v` sets verbose>0 (upstream also raises STATS to 1) and does show it.
    let _ = suppress_updated_only_totals;
    let emit_trailer_totals = !stats_on && verbosity > 0;

    // upstream: main.c:427/458 - `output_summary()` unconditionally emits a
    // leading `rprintf(FCLIENT, "\n")` before both the STATS>=2 detail block and
    // the STATS>=1 sent/received trailer, separating them from any preceding
    // per-file output. When a per-file block (verbose listing, itemize, or
    // progress) was rendered, its trailing separator above already supplied that
    // blank; when nothing preceded (a plain `--stats`, or an empty `-v` run) we
    // still emit exactly one blank so the trailer keeps its upstream framing.
    // `testsuite/itemize.test`'s `v_filt` helper relies on this empty line
    // (`sed -e '/^$/,$d'`) to strip the trailer when matching `-vv` goldens.
    let rendered_block = formatted_rendered || progress_rendered || emit_verbose_listing;
    if (stats_on || emit_trailer_totals) && (emit_verbose_listing || !rendered_block) {
        writeln!(writer)?;
    }

    if stats_on {
        emit_stats(
            summary,
            writer,
            human_readable_mode,
            dry_run,
            only_write_batch,
            stats_level,
            show_copy_method,
        )?;
    } else if emit_trailer_totals {
        emit_totals(
            summary,
            writer,
            human_readable_mode,
            dry_run,
            only_write_batch,
            show_copy_method,
        )?;
    }

    Ok(())
}

/// Renders an atime/crtime column field, right-justified in a width of
/// `1 + len(timestamp)` so a populated value carries one leading space and a
/// blank value fills the whole column with spaces.
///
/// upstream: generator.c list_file_entry() - the atime/crtime fields use the
/// `%*s` positive width `1 + strlen(mtime_str)` (20 for a 19-char timestamp).
fn format_list_time_column(time: Option<std::time::SystemTime>, blank: bool) -> String {
    // The width tracks `format_list_timestamp`'s output length (19 chars
    // "YYYY/MM/DD HH:MM:SS") plus one leading space, so it stays faithful even
    // if the timestamp format changes.
    let width = 1 + format_list_timestamp(Some(std::time::SystemTime::UNIX_EPOCH)).len();
    let value = if blank || time.is_none() {
        String::new()
    } else {
        format_list_timestamp(time)
    };
    format!("{value:>width$}")
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_list_only<W: Write + ?Sized>(
    events: &[ClientEvent],
    stdout: &mut W,
    human_readable: HumanReadableMode,
    show_atimes: bool,
    show_crtimes: bool,
    eight_bit_output: bool,
    preserve_links: bool,
) -> io::Result<()> {
    for event in events {
        if !list_only_event(event.kind()) {
            continue;
        }

        if let Some(metadata) = event.metadata() {
            let permissions = format_list_permissions(metadata);
            let size = format_list_size(metadata.length(), human_readable);
            let timestamp = format_list_timestamp(metadata.modified());
            // upstream: generator.c list_file_entry() - the atime column is
            // blanked for directories (`!S_ISDIR(f->mode)`), while the crtime
            // column is shown for all entry types.
            let atime_field = if show_atimes {
                format_list_time_column(metadata.accessed(), metadata.kind().is_directory())
            } else {
                String::new()
            };
            let crtime_field = if show_crtimes {
                format_list_time_column(metadata.created(), false)
            } else {
                String::new()
            };
            let mut rendered = escape_path(event.relative_path(), eight_bit_output);
            // upstream: generator.c:1183 list_file_entry() - the ` -> <target>`
            // arrow is emitted only when `preserve_links && S_ISLNK(f->mode)`.
            // Without `--links`/`-l` the symlink is still listed (with its
            // target-length size) but no target string.
            if preserve_links
                && metadata.kind().is_symlink()
                && let Some(target) = metadata.symlink_target()
            {
                rendered.extend_from_slice(b" -> ");
                rendered.extend_from_slice(&escape_path(target, eight_bit_output));
            }

            // The columns are ASCII; the filename bytes follow raw so an invalid
            // byte under -8 survives to the sink.
            write!(
                stdout,
                "{permissions} {size} {timestamp}{atime_field}{crtime_field} "
            )?;
            stdout.write_all(&rendered)?;
            stdout.write_all(b"\n")?;
        } else {
            let rendered = escape_path(event.relative_path(), eight_bit_output);
            write!(
                stdout,
                "?????????? {:>width$} {} ",
                "?",
                format_list_timestamp(None),
                width = human_readable.size_width(),
            )?;
            stdout.write_all(&rendered)?;
            stdout.write_all(b"\n")?;
        }
    }

    Ok(())
}

/// Rewrites Windows native backslash separators to POSIX forward slashes.
///
/// upstream: flist.c f_name() emits forward-slash separators regardless of
/// host OS. The escaped relative path carries native backslashes only on
/// Windows; other platforms already use forward slashes, so the binding is
/// returned unchanged.
#[cfg(windows)]
fn normalize_progress_separators(mut name: Vec<u8>) -> Vec<u8> {
    for byte in name.iter_mut() {
        if *byte == b'\\' {
            *byte = b'/';
        }
    }
    name
}

/// Non-Windows paths already use forward-slash separators; returned as-is.
#[cfg(not(windows))]
fn normalize_progress_separators(name: Vec<u8>) -> Vec<u8> {
    name
}

/// Renders progress lines for the provided transfer events.
pub(crate) fn emit_progress<W: Write + ?Sized>(
    events: &[ClientEvent],
    stdout: &mut W,
    human_readable: HumanReadableMode,
    eight_bit_output: bool,
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

    // Denominator counts every checked flist entry (upstream num_files:
    // directories and symlinks included); the numerator `total - checked`
    // counts down over all of them, mirroring upstream's
    // `num_files - current_file_index - 1`. Only regular-file transfers print a
    // block and advance `xfr#` (upstream receiver.c:782), so a symlink or
    // directory is counted but silent.
    let total = flist_entries.len();
    let transferred_total = flist_entries
        .iter()
        .filter(|event| event.kind().is_transfer() && !is_uptodate_event(event))
        .count();
    if transferred_total == 0 {
        return Ok(false);
    }

    let mut xfr_index = 0usize;
    let mut checked = 0usize;
    for event in flist_entries.into_iter() {
        checked += 1;
        if !event.kind().is_transfer() || is_uptodate_event(event) {
            continue;
        }
        xfr_index += 1;
        let remaining = total.saturating_sub(checked);

        let name =
            normalize_progress_separators(escape_path(event.relative_path(), eight_bit_output));
        stdout.write_all(&name)?;
        stdout.write_all(b"\n")?;

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
    only_write_batch: bool,
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

    emit_totals(
        summary,
        stdout,
        human_readable,
        dry_run,
        only_write_batch,
        show_copy_method,
    )
}

/// Builds the five `Number of files` breakdown categories in upstream order.
///
/// upstream: main.c:388 `output_itemized_counts` uses `labels[] = {"reg", "dir",
/// "link", "dev", "special"}`, keeping device nodes (`dev`) split from other
/// specials (`special`: fifos/sockets). Folding the two together diverges from
/// upstream's five-category line.
fn files_count_categories(
    reg: u64,
    dir: u64,
    link: u64,
    dev: u64,
    special: u64,
) -> [(&'static str, u64); 5] {
    [
        ("reg", reg),
        ("dir", dir),
        ("link", link),
        ("dev", dev),
        ("special", special),
    ]
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
    let symlinks_total = summary.symlinks_total();
    let devices_total = summary.devices_total();
    let fifos_total = summary.fifos_total();
    let deleted = summary.items_deleted();
    // upstream: main.c output_itemized_counts("Number of deleted files", ...)
    // prints the total plus a per-type breakdown (reg/dir/link/dev/special),
    // where reg = total - (dir + link + dev + special).
    let deleted_breakdown = format_stat_categories(
        &[
            ("reg", summary.deleted_regular_files()),
            ("dir", summary.deleted_dirs()),
            ("link", summary.deleted_symlinks()),
            ("dev", summary.deleted_devices()),
            ("special", summary.deleted_specials()),
        ],
        human_readable,
    );
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
    let total_entries = files_total
        .saturating_add(directories_total)
        .saturating_add(symlinks_total)
        .saturating_add(special_total);

    // upstream: receiver.c:733-746 / sender.c:295-308 - "Number of created
    // files" counts ITEM_IS_NEW entries per type (new dirs, symlinks, devices,
    // specials and empty files included), NOT the "copied/updated" tallies. An
    // in-place update of a pre-existing file/symlink is transferred but never
    // ITEM_IS_NEW, so it must not inflate the created counts. `created_dirs`
    // uses `directories_created`, already new-only (mkdir + synthesized root).
    let created_reg = summary.created_regular_files();
    let created_symlinks = summary.created_symlinks();
    let created_devices = summary.created_devices();
    let created_specials = summary.created_specials();
    let created_total = created_reg
        .saturating_add(directories)
        .saturating_add(created_symlinks)
        .saturating_add(created_devices)
        .saturating_add(created_specials);

    // upstream: main.c:388 output_itemized_counts labels[] =
    // {reg, dir, link, dev, special} - devices ('dev') are counted separately
    // from other specials (fifos/sockets), never folded together.
    let files_breakdown = format_stat_categories(
        &files_count_categories(
            files_total,
            directories_total,
            symlinks_total,
            devices_total,
            fifos_total,
        ),
        human_readable,
    );
    // upstream: main.c output_itemized_counts labels the created breakdown
    // reg/dir/link/dev/special, with devices ('dev') split from other specials.
    let created_breakdown = format_stat_categories(
        &[
            ("reg", created_reg),
            ("dir", directories),
            ("link", created_symlinks),
            ("dev", created_devices),
            ("special", created_specials),
        ],
        human_readable,
    );

    let total_size_display = format_size(total_size, human_readable);
    let transferred_size_display = format_size(transferred_size, human_readable);
    let literal_bytes_display = format_size(literal_bytes, human_readable);
    let matched_bytes_display = format_size(matched_bytes, human_readable);
    let file_list_size_display = format_size(file_list_size, human_readable);
    let bytes_sent_display = format_size(bytes_sent, human_readable);
    let bytes_received_display = format_size(bytes_received, human_readable);

    // upstream: main.c output_summary() - every count is wrapped in comma_num(),
    // e.g. `rprintf(FINFO, "Number of files: %s%s\n",
    // comma_num(stats.num_files), ...)`. comma_num = do_big_num(num,
    // human_readable != 0, NULL) (inums.h), so a count is comma-grouped at every
    // enabled level (`1,500`) but rendered as raw digits under --no-h (`1500`);
    // counts are never humanised to K/M/G units, even at -hh.
    writeln!(
        stdout,
        "Number of files: {}{files_breakdown}",
        format_count(total_entries, human_readable)
    )?;
    // upstream: main.c:429 - `if (protocol_version >= 29)`
    if summary.protocol_version() >= 29 {
        writeln!(
            stdout,
            "Number of created files: {}{created_breakdown}",
            format_count(created_total, human_readable)
        )?;
    }
    // upstream: main.c:431 - `if (protocol_version >= 31)`
    if summary.protocol_version() >= 31 {
        writeln!(
            stdout,
            "Number of deleted files: {}{deleted_breakdown}",
            format_count(deleted, human_readable)
        )?;
    }
    writeln!(
        stdout,
        "Number of regular files transferred: {}",
        format_count(files, human_readable)
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
    only_write_batch: bool,
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
    // upstream: main.c:469 - `write_batch < 0 ? " (BATCH ONLY)" : dry_run ?
    // " (DRY RUN)" : ""`. `--only-write-batch` sets `write_batch < 0` and takes
    // precedence over `--dry-run`.
    let speedup_suffix = if only_write_batch {
        " (BATCH ONLY)"
    } else if dry_run {
        " (DRY RUN)"
    } else {
        ""
    };
    // upstream: main.c:466-468 - speedup uses comma_dnum(_, 2), i.e. thousands
    // grouping. Reuse the same helper the --stats path uses (stats_format.rs)
    // so both summary paths group identically.
    let speedup_display = crate::stats_format::format_speedup(speedup);
    writeln!(
        stdout,
        "total size is {total_size_display}  speedup is {speedup_display}{speedup_suffix}"
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

/// Returns `true` for the per-file skip notices that upstream's `recv_generator`
/// emits during the generator phase (`"exists"`, `"not creating new"`, `"is
/// newer"`, `"is over max-size"`, `"is under min-size"`), ahead of the receiver
/// phase that prints transferred-file names. Bucketing these with the uptodate
/// notices reproduces that interleaving.
///
/// upstream: generator.c:1379,1395,1708,1716,1723 (recv_generator)
fn is_generator_phase_skip(event: &ClientEvent) -> bool {
    matches!(
        event.kind(),
        ClientEventKind::SkippedExisting
            | ClientEventKind::SkippedMissingDestination
            | ClientEventKind::SkippedNewerDestination
            | ClientEventKind::SkippedOverMaxSize
            | ClientEventKind::SkippedUnderMinSize
    )
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
fn verbose_listing_name(event: &ClientEvent, eight_bit_output: bool) -> Vec<u8> {
    let path = event.relative_path();
    let is_dir = matches!(
        event.metadata().map(ClientEntryMetadata::kind),
        Some(ClientEntryKind::Directory)
    );
    if is_dir && path.as_os_str() == "." {
        return b"./".to_vec();
    }
    let mut rendered = escape_path(path, eight_bit_output);
    // upstream: flist.c f_name() emits POSIX forward-slash separators
    // regardless of host OS. Normalize Windows native backslashes at the
    // rendering boundary; storage retains the platform-native form.
    #[cfg(windows)]
    for byte in rendered.iter_mut() {
        if *byte == b'\\' {
            *byte = b'/';
        }
    }
    if is_dir {
        rendered.push(b'/');
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
    eight_bit_output: bool,
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
        // upstream: recv_generator() emits the "is uptodate", "exists",
        // "not creating new", and "is newer" notices synchronously in the
        // generator phase, ahead of the receiver phase that prints the
        // transferred-file names. Bucket those generator-phase notices first
        // so the stable sort reproduces that interleaving.
        // (generator.c:1379,1395,1723)
        if is_uptodate_event(event) || is_generator_phase_skip(event) {
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
                writeln_wrapped(
                    stdout,
                    "",
                    event.relative_path(),
                    eight_bit_output,
                    " is uptodate",
                )?;
                continue;
            }

            let mut rendered = verbose_listing_name(event, eight_bit_output);
            if matches!(kind, ClientEventKind::SymlinkCopied)
                && let Some(metadata) = event.metadata()
                && let Some(target) = metadata.symlink_target()
            {
                rendered.extend_from_slice(b" -> ");
                rendered.extend_from_slice(&escape_path(target, eight_bit_output));
            }
            stdout.write_all(&rendered)?;
            stdout.write_all(b"\n")?;
            continue;
        }

        if name_overridden && !include_for_name {
            continue;
        }

        match kind {
            ClientEventKind::SkippedExisting => {
                // upstream: generator.c:1397-1409 - `rprintf(FINFO, "%s exists\n",
                // fname)` for an --ignore-existing skip: the bare relative name
                // followed by " exists", no descriptor and no quotes. Gated on
                // INFO_GTE(SKIP, 1), which the info verbosity table raises only
                // at -vv (options.c:252).
                if info_gte(InfoFlag::Skip, 1) {
                    writeln_wrapped(
                        stdout,
                        "",
                        event.relative_path(),
                        eight_bit_output,
                        " exists",
                    )?;
                }
                continue;
            }
            ClientEventKind::SkippedMissingDestination => {
                // upstream: generator.c:1379-1382 - `rprintf(FINFO,
                // "not creating new %s \"%s\"\n", "file", fname)` for an
                // --existing / --ignore-non-existing skip, gated on
                // INFO_GTE(SKIP, 1).
                if info_gte(InfoFlag::Skip, 1) {
                    writeln_wrapped(
                        stdout,
                        "not creating new file \"",
                        event.relative_path(),
                        eight_bit_output,
                        "\"",
                    )?;
                }
                continue;
            }
            ClientEventKind::SkippedNewerDestination => {
                // upstream: generator.c:1723-1724 - `rprintf(FINFO, "%s is
                // newer\n", fname)` for an --update skip: the bare relative name
                // followed by " is newer", gated on INFO_GTE(SKIP, 1).
                if info_gte(InfoFlag::Skip, 1) {
                    writeln_wrapped(
                        stdout,
                        "",
                        event.relative_path(),
                        eight_bit_output,
                        " is newer",
                    )?;
                }
                continue;
            }
            ClientEventKind::SkippedOverMaxSize => {
                // upstream: generator.c:1704-1711 - `rprintf(FINFO,
                // "%s is over max-size\n", fname)` for a `--max-size` skip: the
                // bare relative name followed by " is over max-size", gated on
                // INFO_GTE(SKIP, 1).
                if info_gte(InfoFlag::Skip, 1) {
                    writeln_wrapped(
                        stdout,
                        "",
                        event.relative_path(),
                        eight_bit_output,
                        " is over max-size",
                    )?;
                }
                continue;
            }
            ClientEventKind::SkippedUnderMinSize => {
                // upstream: generator.c:1712-1719 - `rprintf(FINFO,
                // "%s is under min-size\n", fname)` for a `--min-size` skip: the
                // bare relative name followed by " is under min-size", gated on
                // INFO_GTE(SKIP, 1).
                if info_gte(InfoFlag::Skip, 1) {
                    writeln_wrapped(
                        stdout,
                        "",
                        event.relative_path(),
                        eight_bit_output,
                        " is under min-size",
                    )?;
                }
                continue;
            }
            ClientEventKind::SkippedNonRegular => {
                writeln_wrapped(
                    stdout,
                    "skipping non-regular file \"",
                    event.relative_path(),
                    eight_bit_output,
                    "\"",
                )?;
                continue;
            }
            ClientEventKind::SkippedDirectory => {
                // upstream: flist.c:1338 and flist.c:2452 -
                // `rprintf(FINFO, "skipping directory %s\n", ...)`: the bare
                // relative name with no surrounding quotes and no trailing
                // "(no recursion)" suffix.
                writeln_wrapped(
                    stdout,
                    "skipping directory ",
                    event.relative_path(),
                    eight_bit_output,
                    "",
                )?;
                continue;
            }
            ClientEventKind::SkippedUnsafeSymlink => {
                let mut rendered = b"ignoring unsafe symlink \"".to_vec();
                rendered.extend_from_slice(&escape_path(event.relative_path(), eight_bit_output));
                rendered.push(b'"');
                if let Some(metadata) = event.metadata()
                    && let Some(target) = metadata.symlink_target()
                {
                    rendered.extend_from_slice(b" -> ");
                    rendered.extend_from_slice(&escape_path(target, eight_bit_output));
                }
                stdout.write_all(&rendered)?;
                stdout.write_all(b"\n")?;
                continue;
            }
            ClientEventKind::SkippedMountPoint => {
                writeln_wrapped(
                    stdout,
                    "skipping mount point \"",
                    event.relative_path(),
                    eight_bit_output,
                    "\"",
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
                    writeln_wrapped(
                        stdout,
                        "",
                        event.relative_path(),
                        eight_bit_output,
                        " is uptodate",
                    )?;
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
                    writeln_wrapped(
                        stdout,
                        "",
                        event.relative_path(),
                        eight_bit_output,
                        " is uptodate",
                    )?;
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
                    writeln_wrapped(
                        stdout,
                        "",
                        event.relative_path(),
                        eight_bit_output,
                        " is uptodate",
                    )?;
                }
                continue;
            }
            // upstream: generator.c:1145-1147 - a `--link-dest` symlink
            // hard-linked from the basis prints `"%s is uptodate"` at NAME>=2.
            ClientEventKind::HardLink if is_hardlinked_symlink_event(event) => {
                if verbosity >= 2 || matches!(name_level, NameOutputLevel::UpdatedAndUnchanged) {
                    writeln_wrapped(
                        stdout,
                        "",
                        event.relative_path(),
                        eight_bit_output,
                        " is uptodate",
                    )?;
                }
                continue;
            }
            ClientEventKind::DirectoryCreated
                if !event.was_created() && !event.change_set().has_any_change() =>
            {
                if verbosity >= 2 || matches!(name_level, NameOutputLevel::UpdatedAndUnchanged) {
                    writeln_wrapped(
                        stdout,
                        "",
                        event.relative_path(),
                        eight_bit_output,
                        "/ is uptodate",
                    )?;
                }
                continue;
            }
            ClientEventKind::SymlinkCopied
                if !event.was_created() && !event.change_set().has_any_change() =>
            {
                if verbosity >= 2 || matches!(name_level, NameOutputLevel::UpdatedAndUnchanged) {
                    writeln_wrapped(
                        stdout,
                        "",
                        event.relative_path(),
                        eight_bit_output,
                        " is uptodate",
                    )?;
                }
                continue;
            }
            _ => {}
        }

        let mut rendered = verbose_listing_name(event, eight_bit_output);
        if matches!(kind, ClientEventKind::SymlinkCopied)
            && let Some(metadata) = event.metadata()
            && let Some(target) = metadata.symlink_target()
        {
            rendered.extend_from_slice(b" -> ");
            rendered.extend_from_slice(&escape_path(target, eight_bit_output));
        } else if matches!(kind, ClientEventKind::HardLink)
            && let Some(metadata) = event.metadata()
            && let Some(leader) = metadata.symlink_target()
        {
            // upstream: hlink.c:236 - a freshly-linked alias prints
            // `"%s => %s"` with the group leader's relative path.
            rendered.extend_from_slice(b" => ");
            rendered.extend_from_slice(&escape_path(leader, eight_bit_output));
        }

        // upstream: log.c:log_formatted() emits the default `%n%L` per-file
        // line at every verbosity tier (set in options.c:2372). The rendered
        // bytes already include the `-> target` suffix for symlinks; higher
        // tiers only add ancillary log messages, never a per-file descriptor
        // prefix or byte-count wrapper.
        stdout.write_all(&rendered)?;
        stdout.write_all(b"\n")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::local_copy::LocalCopyChangeSet;
    use std::path::PathBuf;

    fn render_verbose(event: ClientEvent) -> String {
        let mut out = Vec::new();
        emit_verbose(
            &[event],
            1,
            NameOutputLevel::UpdatedAndUnchanged,
            false,
            HumanReadableMode::Grouped,
            false,
            &mut out,
        )
        .expect("emit_verbose writes to an in-memory buffer");
        String::from_utf8(out).expect("output is valid UTF-8")
    }

    fn render_verbose_scenario(events: Vec<ClientEvent>, verbose_level: u8) -> String {
        // Mirror the CLI's per-thread verbosity setup so `info_gte(Skip, ..)`
        // reflects the requested level. `from_verbose_level(2)` raises Skip to 1
        // (upstream options.c:252), `(1)` leaves it at 0.
        logging::init(logging::VerbosityConfig::from_verbose_level(verbose_level));
        let mut out = Vec::new();
        emit_verbose(
            &events,
            verbose_level,
            NameOutputLevel::UpdatedAndUnchanged,
            false,
            HumanReadableMode::Grouped,
            false,
            &mut out,
        )
        .expect("emit_verbose writes to an in-memory buffer");
        String::from_utf8(out).expect("output is valid UTF-8")
    }

    fn transferred_event(name: &str) -> ClientEvent {
        ClientEvent::for_test(
            PathBuf::from(name),
            ClientEventKind::DataCopied,
            true,
            Some(ClientEntryMetadata::for_test(ClientEntryKind::File)),
            LocalCopyChangeSet::new(),
        )
    }

    fn skip_event(name: &str, kind: ClientEventKind) -> ClientEvent {
        ClientEvent::for_test(
            PathBuf::from(name),
            kind,
            false,
            Some(ClientEntryMetadata::for_test(ClientEntryKind::File)),
            LocalCopyChangeSet::new(),
        )
    }

    #[test]
    fn ignore_existing_skip_notices_cluster_before_transferred_at_vv() {
        // WHY: a drop-in tool parses the local client stream in order. Upstream's
        // recv_generator emits the `"%s exists"` --ignore-existing notice in the
        // generator phase, ahead of the receiver phase that prints transferred
        // names, and only at INFO_GTE(SKIP, 1) (i.e. -vv). Collection order here
        // is the sorted flist (big, onlyexists, small, tiny); the two skip
        // notices must surface first, in flist order, with upstream's exact
        // bare-name-plus-" exists" text - never after the transferred names or
        // after the stats block.
        // upstream: generator.c:1395-1409
        let events = vec![
            transferred_event("big.txt"),
            skip_event("onlyexists.txt", ClientEventKind::SkippedExisting),
            skip_event("small.txt", ClientEventKind::SkippedExisting),
            transferred_event("tiny.txt"),
        ];
        assert_eq!(
            render_verbose_scenario(events, 2),
            "onlyexists.txt exists\nsmall.txt exists\nbig.txt\ntiny.txt\n"
        );
    }

    #[test]
    fn skip_notices_suppressed_below_skip_verbosity() {
        // WHY: upstream gates the exists/not-creating/is-newer notices on
        // INFO_GTE(SKIP, 1), which the info verbosity table raises only at -vv
        // (options.c:252). At plain -v they must be silent, leaving only the
        // transferred-file names - otherwise a drop-in tool sees phantom lines
        // that upstream never emits.
        let events = vec![
            transferred_event("big.txt"),
            skip_event("onlyexists.txt", ClientEventKind::SkippedExisting),
            skip_event("small.txt", ClientEventKind::SkippedExisting),
            transferred_event("tiny.txt"),
        ];
        assert_eq!(render_verbose_scenario(events, 1), "big.txt\ntiny.txt\n");
    }

    #[test]
    fn not_creating_and_is_newer_use_upstream_text_at_vv() {
        // WHY: the local path previously emitted oc-invented wording ("skipping
        // non-existent destination file", "skipping newer destination file").
        // Upstream prints `not creating new file "%s"` (generator.c:1380) and
        // `%s is newer` (generator.c:1724). Drop-in parsers key on the exact
        // strings, so any deviation breaks compatibility.
        let missing = vec![skip_event(
            "big.txt",
            ClientEventKind::SkippedMissingDestination,
        )];
        assert_eq!(
            render_verbose_scenario(missing, 2),
            "not creating new file \"big.txt\"\n"
        );
        let newer = vec![skip_event(
            "big.txt",
            ClientEventKind::SkippedNewerDestination,
        )];
        assert_eq!(render_verbose_scenario(newer, 2), "big.txt is newer\n");
    }

    #[test]
    fn size_skip_notices_cluster_before_transferred_at_vv() {
        // WHY: upstream recv_generator emits the `"%s is over max-size"` /
        // `"%s is under min-size"` notices in the generator phase, ahead of the
        // receiver phase that prints transferred names, and only at
        // INFO_GTE(SKIP, 1) (i.e. -vv). A drop-in tool parses the stream in
        // order, so the two size-skip notices must surface first, in flist
        // order, with upstream's exact bare-name text - never after the
        // transferred names or after the stats block.
        // upstream: generator.c:1704-1719
        let events = vec![
            transferred_event("keep.txt"),
            skip_event("big.bin", ClientEventKind::SkippedOverMaxSize),
            skip_event("tiny.bin", ClientEventKind::SkippedUnderMinSize),
            transferred_event("also.txt"),
        ];
        assert_eq!(
            render_verbose_scenario(events, 2),
            "big.bin is over max-size\ntiny.bin is under min-size\nkeep.txt\nalso.txt\n"
        );
    }

    #[test]
    fn size_skip_notices_suppressed_below_skip_verbosity() {
        // WHY: upstream gates the size-skip notices on INFO_GTE(SKIP, 1), which
        // the info verbosity table raises only at -vv (options.c:252). At plain
        // -v they must be silent, leaving only the transferred name - otherwise
        // a drop-in tool sees phantom lines upstream never emits.
        let events = vec![
            transferred_event("keep.txt"),
            skip_event("big.bin", ClientEventKind::SkippedOverMaxSize),
            skip_event("tiny.bin", ClientEventKind::SkippedUnderMinSize),
        ];
        assert_eq!(render_verbose_scenario(events, 1), "keep.txt\n");
    }

    #[test]
    fn size_skips_and_6639_notices_share_generator_phase_bucket() {
        // WHY: the size-skip notices extend the same generator-phase bucket that
        // #6639 established for the "exists" / "not creating new" / "is newer"
        // notices. Mixing all of them must keep every generator-phase notice
        // ahead of the transferred names, in flist order, so adding the size
        // cases does not regress the #6639 ordering.
        // upstream: generator.c:1379,1395,1708,1716,1723
        let events = vec![
            transferred_event("keep.txt"),
            skip_event("exists.txt", ClientEventKind::SkippedExisting),
            skip_event("big.bin", ClientEventKind::SkippedOverMaxSize),
            skip_event("newer.txt", ClientEventKind::SkippedNewerDestination),
            skip_event("tiny.bin", ClientEventKind::SkippedUnderMinSize),
        ];
        assert_eq!(
            render_verbose_scenario(events, 2),
            "exists.txt exists\nbig.bin is over max-size\nnewer.txt is newer\n\
             tiny.bin is under min-size\nkeep.txt\n"
        );
    }

    #[test]
    fn skipped_directory_matches_upstream_bare_message() {
        let event = ClientEvent::for_test(
            PathBuf::from("subdir"),
            ClientEventKind::SkippedDirectory,
            false,
            Some(ClientEntryMetadata::for_test(ClientEntryKind::Directory)),
            LocalCopyChangeSet::new(),
        );
        // upstream: flist.c:1338 and flist.c:2452 emit
        // `rprintf(FINFO, "skipping directory %s\n", ...)` - a bare relative
        // name with no surrounding quotes and no "(no recursion)" suffix. Byte
        // fidelity with upstream requires exactly this form.
        assert_eq!(render_verbose(event), "skipping directory subdir\n");
    }

    /// Renders the summary trailer for an empty (default) transfer at the given
    /// stats level with the requested `--dry-run` / `--only-write-batch` flags.
    fn render_summary(level: u8, dry_run: bool, only_write_batch: bool) -> String {
        let summary = ClientSummary::default();
        let mut out = Vec::new();
        emit_transfer_summary(
            &summary,
            0,     // verbosity
            None,  // progress_mode
            level, // stats_level
            false, // progress_already_rendered
            false, // list_only
            dry_run,
            only_write_batch,
            None, // out_format
            &OutFormatContext::default(),
            NameOutputLevel::Disabled,
            false, // name_overridden
            HumanReadableMode::Grouped,
            false, // suppress_updated_only_totals
            false, // emit_flist_banner
            false, // show_copy_method
            false, // show_atimes
            false, // show_crtimes
            false, // eight_bit_output
            &mut out,
        )
        .expect("emit_transfer_summary writes to an in-memory buffer");
        String::from_utf8(out).expect("output is valid UTF-8")
    }

    #[test]
    fn plain_stats_trailer_starts_with_blank_line() {
        // upstream: main.c:458 - output_summary() emits an unconditional leading
        // `rprintf(FCLIENT, "\n")` before the STATS>=1 sent/received trailer,
        // even when no per-file output preceded it (a plain `--stats` run). oc
        // previously emitted the trailer with no leading blank.
        let out = render_summary(1, false, false);
        assert!(
            out.starts_with('\n'),
            "plain --stats trailer must start with a blank line:\n{out:?}"
        );
        assert!(out.contains("sent "));
        assert!(out.contains("total size is"));
        // Exactly one leading blank - not a doubled empty line.
        assert!(
            !out.starts_with("\n\n"),
            "leading blank must not double:\n{out:?}"
        );
    }

    #[test]
    fn stats_detail_block_starts_with_blank_line() {
        // upstream: main.c:427 - the STATS>=2 detail block is also preceded by an
        // unconditional leading blank.
        let out = render_summary(2, false, false);
        assert!(
            out.starts_with('\n'),
            "stats block must start blank:\n{out:?}"
        );
        assert!(
            !out.starts_with("\n\n"),
            "leading blank must not double:\n{out:?}"
        );
        assert!(out.contains("Number of files:"));
    }

    #[test]
    fn speedup_suffix_matches_upstream_precedence() {
        // upstream: main.c:469 - `write_batch < 0 ? " (BATCH ONLY)" : dry_run ?
        // " (DRY RUN)" : ""`. --only-write-batch wins over --dry-run.
        assert!(render_summary(1, false, false).contains("speedup is"));
        assert!(!render_summary(1, false, false).contains("(DRY RUN)"));
        assert!(!render_summary(1, false, false).contains("(BATCH ONLY)"));

        assert!(render_summary(1, true, false).contains(" (DRY RUN)"));

        let batch = render_summary(1, false, true);
        assert!(batch.contains(" (BATCH ONLY)"), "{batch:?}");
        assert!(!batch.contains("(DRY RUN)"), "{batch:?}");

        // Precedence: batch beats dry-run when both are set.
        let both = render_summary(1, true, true);
        assert!(both.contains(" (BATCH ONLY)"), "{both:?}");
        assert!(!both.contains("(DRY RUN)"), "{both:?}");
    }

    #[test]
    fn files_count_categories_split_dev_from_special() {
        // upstream: main.c:388 output_itemized_counts labels[] =
        // {reg, dir, link, dev, special}. Device nodes ('dev') must be counted
        // separately from other specials (fifos/sockets); folding them into a
        // single 'special' count diverges from upstream's five-category line.
        let cats = files_count_categories(3, 2, 1, 4, 5);
        assert_eq!(
            cats,
            [
                ("reg", 3),
                ("dir", 2),
                ("link", 1),
                ("dev", 4),
                ("special", 5)
            ]
        );
        assert_eq!(
            format_stat_categories(&cats, HumanReadableMode::Grouped),
            " (reg: 3, dir: 2, link: 1, dev: 4, special: 5)"
        );
    }
}

#![deny(unsafe_code)]

//! Rendering helpers for parsed `--out-format` specifications.

mod checksum;
mod format;
mod itemize;
mod placeholder;
#[cfg(test)]
mod tests;

use std::io::{self, Write};

use core::client::{ClientEntryKind, ClientEntryMetadata, ClientEvent, ClientEventKind};
use logging::{InfoFlag, info_gte};

use crate::frontend::escape::escape_path;

use super::tokens::{OutFormat, OutFormatContext, OutFormatToken};

use format::apply_placeholder_format;
use placeholder::render_placeholder_value;

impl OutFormat {
    /// Renders an event according to the parsed `--out-format` tokens.
    pub(crate) fn render<W: Write + ?Sized>(
        &self,
        event: &ClientEvent,
        context: &OutFormatContext,
        writer: &mut W,
    ) -> io::Result<()> {
        let mut buffer = String::new();
        for token in self.tokens() {
            match token {
                OutFormatToken::Literal(text) => buffer.push_str(text),
                OutFormatToken::Placeholder(spec) => {
                    if let Some(rendered) = render_placeholder_value(event, context, spec) {
                        let formatted = apply_placeholder_format(rendered, &spec.format);
                        buffer.push_str(&formatted);
                    }
                }
            }
        }

        if buffer.ends_with('\n') {
            writer.write_all(buffer.as_bytes())
        } else {
            writer.write_all(buffer.as_bytes())?;
            writer.write_all(b"\n")
        }
    }
}

/// Returns `true` when the event should be suppressed from `--out-format` output.
///
/// Mirrors the upstream emit gate in `generator.c:582-583`: when `iflags == 0`
/// (no significant attribute changes and the file was not transferred), the
/// itemize line is suppressed UNLESS `INFO_GTE(NAME, 2)` is in effect (`-vv`
/// or `--info=name2`). In the local-copy path the unchanged case corresponds
/// to `MetadataReused` events whose `change_set` reports no changes and that
/// were not newly created.
///
/// upstream: generator.c:582-583 - emit when `iflags & (SIGNIFICANT_ITEM_FLAGS
/// | ITEM_REPORT_XATTR) || INFO_GTE(NAME, 2) || stdout_format_has_i > 1
/// || (xname && *xname)`.
fn should_suppress_event(event: &ClientEvent, context: &OutFormatContext) -> bool {
    if context.emit_unchanged() || context.itemize_repeated() {
        // upstream: generator.c:582-583 - two separate arms force the itemize
        // line for unchanged entries: `INFO_GTE(NAME, 2)` (`-vv`, threaded as
        // `emit_unchanged`) and `stdout_format_has_i > 1` (`-ii`, threaded as
        // `itemize_repeated`). Either one surfaces dirs, files, and symlinks
        // that match the source exactly as all-dot rows.
        return false;
    }
    if matches!(event.kind(), ClientEventKind::MetadataReused)
        && !event.was_created()
        && !event.change_set().has_any_change()
    {
        return true;
    }

    // upstream: generator.c:582-583 + rsync.h:258 - a `--copy-dest`
    // reconstruction that exactly matches the basis is itemized with only
    // ITEM_LOCAL_CHANGE, which is excluded from `SIGNIFICANT_ITEM_FLAGS`. At
    // plain `-i` (NAME < 2, no `-ii`) the emit gate therefore drops the row.
    // A directory, symlink, fifo, device, or regular-file copy reconstructed
    // from the basis with no attribute drift and no creation is suppressed;
    // only the `-vv` path (`emit_unchanged`, handled above) surfaces it. The
    // symlink's own `-> target` is the `%L` field, not an xname, so it does NOT
    // force emission - only a hardlink alias's `=> leader` xname does, which the
    // dedicated HardLink rule below preserves.
    if matches!(
        event.kind(),
        ClientEventKind::ReferenceCopied
            | ClientEventKind::DirectoryCreated
            | ClientEventKind::SymlinkCopied
            | ClientEventKind::FifoCopied
            | ClientEventKind::DeviceCopied
    ) && !event.was_created()
        && !event.change_set().has_any_change()
    {
        return true;
    }

    // upstream: generator.c:1119-1147 - a `--link-dest` symlink hard-linked
    // from the basis (`hL`) carries only ITEM_LOCAL_CHANGE and no `=> leader`
    // xname, so it is suppressed at plain `-i`. Its `-> target` is the `%L`
    // field, not an xname, so it does not force emission.
    if matches!(event.kind(), ClientEventKind::HardLink)
        && !event.was_created()
        && !event.change_set().has_any_change()
        && event
            .metadata()
            .map(ClientEntryMetadata::kind)
            .is_some_and(|kind| matches!(kind, ClientEntryKind::Symlink))
    {
        return true;
    }

    // upstream: hlink.c:215-227 + generator.c:581-583 - a hardlink alias is
    // itemized via `itemize(..., ITEM_LOCAL_CHANGE | ITEM_XNAME_FOLLOWS, 0,
    // xname)`. The generator writes the row only when a significant attribute
    // flag is set, `INFO_GTE(NAME, 2)` (handled above via `emit_unchanged`),
    // `-ii`, or the alias was freshly atomic_create'd with a non-empty xname.
    // Two blank cases are suppressed at plain `-i`:
    //   1. An alias with no attribute change and no `=> leader` trailer (empty
    //      xname) - a fresh `--link-dest`/basis hardlink.
    //   2. An already-shared-inode cluster alias (`is_hardlink_uptodate`): even
    //      though it carries a `=> leader` trailer for the `-vv` view, upstream
    //      itemizes it with an EMPTY xname (hlink.c:219-221), so the plain-`-i`
    //      gate drops it.
    // A hard-linked symlink's `-> target` is handled by the earlier Symlink rule.
    if !matches!(event.kind(), ClientEventKind::HardLink)
        || event.was_created()
        || event.change_set().has_any_change()
    {
        return false;
    }
    event.is_hardlink_uptodate()
        || event
            .metadata()
            .and_then(ClientEntryMetadata::symlink_target)
            .is_none()
}

/// Emits each event using the supplied `--out-format` specification.
pub(crate) fn emit_out_format<W: Write + ?Sized>(
    events: &[ClientEvent],
    format: &OutFormat,
    context: &OutFormatContext,
    writer: &mut W,
) -> io::Result<()> {
    for event in events {
        // upstream: generator.c:1721-1724 - a file skipped by `--update`
        // because the destination is newer never reaches the itemize call.
        // The generator emits `"%s is newer"` only at INFO_GTE(SKIP, 1) and
        // then `goto cleanup`, so the itemized row is suppressed regardless
        // of `-i`/`-ii`.
        if matches!(event.kind(), ClientEventKind::SkippedNewerDestination) {
            if info_gte(InfoFlag::Skip, 1) {
                writeln!(
                    writer,
                    "{} is newer",
                    escape_path(event.relative_path(), context.eight_bit_output)
                )?;
            }
            continue;
        }
        if should_suppress_event(event, context) {
            continue;
        }
        format.render(event, context, writer)?;
    }
    Ok(())
}

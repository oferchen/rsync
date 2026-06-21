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
    if context.emit_unchanged() {
        // upstream: generator.c:582 - the `INFO_GTE(NAME, 2)` arm forces the
        // itemize line for unchanged entries so `-vv` surfaces dirs, files,
        // and symlinks that match the source exactly.
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

    // upstream: hlink.c:215-227 + generator.c:581-583 - an already-correct
    // hardlink alias is itemized via `itemize(..., ITEM_LOCAL_CHANGE |
    // ITEM_XNAME_FOLLOWS, 0, "")` with an EMPTY xname. The generator only
    // writes that row when a significant attribute flag is set, `INFO_GTE(NAME,
    // 2)` (handled above via `emit_unchanged`), `stdout_format_has_i > 1`
    // (`-ii`), or the xname is non-empty (a relink occurred). For an up-to-date
    // alias with no attribute change, empty xname, and plain `-i`, all of those
    // are false, so the row is suppressed. The shared inode means config1's
    // perm fix already reached the alias, so itemizing `foo/extra` would
    // over-report. Detect that case: a hardlink-uptodate record with an empty
    // change-set and no `=> target` xname (None symlink target).
    matches!(event.kind(), ClientEventKind::HardLink)
        && event.is_hardlink_uptodate()
        && !event.was_created()
        && !event.change_set().has_any_change()
        && event
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
        if should_suppress_event(event, context) {
            continue;
        }
        format.render(event, context, writer)?;
    }
    Ok(())
}

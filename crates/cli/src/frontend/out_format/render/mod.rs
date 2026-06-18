#![deny(unsafe_code)]

//! Rendering helpers for parsed `--out-format` specifications.

mod checksum;
mod format;
mod itemize;
mod placeholder;
#[cfg(test)]
mod tests;

use std::io::{self, Write};

use core::client::{ClientEvent, ClientEventKind};

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
    matches!(event.kind(), ClientEventKind::MetadataReused)
        && !event.was_created()
        && !event.change_set().has_any_change()
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

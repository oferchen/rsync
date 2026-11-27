//! The two lines a fatal filter refusal prints before the process exits.

use std::io::{self, Write};

use crate::branding::Brand;
use crate::message::strings::exit_code_message;
use crate::message::{Message, Role};
use logging::escape::{EscapeStyle, escape_for_output};

/// Writes `refusal` as upstream reports a fatal filter error and returns its
/// exit code.
///
/// The cause line comes first. A refusal that carries a role is upstream's
/// `rsyserr()` line, `<program>: [<who>] <text>` (log.c:565-590); one without
/// is a bare `rprintf(FERROR, ...)` line such as filter_rule_err()'s
/// (exclude.c:132-136). `log_exit()`'s trailer follows, tagged with `who`, as
/// the `exit_cleanup()` after either one prints it (log.c:1025, cleanup.c:103).
///
/// `who` is upstream's `who_am_i()` for the process that parsed the rule: the
/// client, a remote-shell server, or a daemon's sender or pre-fork receiver.
///
/// `style` escapes the cause line, which echoes operator- or peer-supplied
/// rule text: upstream's `rwrite()` passes it through `filtered_fwrite()`
/// (log.c:476, log.c:499), so a VT in a rule prints as `\#013`. A caller
/// whose writer escapes on its own (a log file) passes
/// `EscapeStyle::passthrough()`.
///
/// # Errors
///
/// Any error writing to `out`.
pub fn write_refusal<W: Write>(
    out: &mut W,
    brand: Brand,
    who: Role,
    refusal: &Message,
    style: EscapeStyle,
) -> io::Result<i32> {
    let code = refusal.code().unwrap_or(1);
    let mut line = Vec::new();
    if refusal.role().is_some() {
        write!(line, "{}: [{}] ", brand.client_program_name(), who.as_str())?;
    }
    line.extend_from_slice(&escape_for_output(refusal.text().as_bytes(), style));
    line.push(b'\n');
    out.write_all(&line)?;
    if let Some(template) = exit_code_message(code) {
        let mut trailer = template.to_message().with_role(who).with_brand(brand);
        if let Some(source) = refusal.source() {
            trailer = trailer.with_source(source.clone());
        }
        trailer.render_line_to_writer(out)?;
    }
    Ok(code)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rsync_error;

    fn rendered_with(who: Role, refusal: &Message, style: EscapeStyle) -> (String, i32) {
        let mut out = Vec::new();
        let code = write_refusal(&mut out, Brand::Upstream, who, refusal, style).expect("write");
        (String::from_utf8(out).expect("utf8"), code)
    }

    fn rendered(who: Role, refusal: &Message) -> (String, i32) {
        rendered_with(who, refusal, EscapeStyle::terminal(false))
    }

    /// upstream: exclude.c:1714-1719 - an operator-named merge file that will
    /// not open is `rsyserr()` (`rsync: [who] ...`) followed by log_exit()'s
    /// RERR_FILEIO trailer, two lines where oc used to print one.
    #[test]
    fn an_open_failure_prints_the_rsyserr_line_and_the_trailer() {
        let refusal = rsync_error!(
            11,
            "failed to open exclude file nope: No such file or directory (2)"
        )
        .with_role(Role::Client);
        let (text, code) = rendered(Role::Sender, &refusal);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(code, 11);
        assert_eq!(lines.len(), 2, "{text}");
        assert_eq!(
            lines[0],
            "rsync: [sender] failed to open exclude file nope: No such file or directory (2)"
        );
        assert!(
            lines[1].starts_with("rsync error: error in file IO (code 11) at "),
            "{text}"
        );
        assert!(lines[1].contains("[sender="), "{text}");
    }

    /// upstream: exclude.c:132-136 filter_rule_err() and the file-named open
    /// failure (exclude.c:1710-1712) are `rprintf(FERROR, ...)`, so their line
    /// names no process: only the trailer does.
    #[test]
    fn a_refusal_without_a_role_prints_a_bare_line() {
        let refusal = rsync_error!(1, "Unknown filter rule: x y");
        let (text, code) = rendered(Role::Server, &refusal);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(code, 1);
        assert_eq!(lines[0], "Unknown filter rule: x y");
        assert!(
            lines[1].starts_with("rsync error: syntax or usage error (code 1) at "),
            "{text}"
        );
        assert!(lines[1].contains("[server="), "{text}");
    }

    /// upstream: exclude.c:1376 echoes the refused rule through
    /// `rprintf(FERROR, ...)`, and log.c:499 `filtered_fwrite()` escapes it,
    /// so a VT in the rule (and in the `'%c'` detail) prints as `\#013`.
    /// Measured against rsync 3.5.2: `--filter=$'+\vfoo'` prints
    /// `invalid modifier '\#013' at position 1 in filter rule: +\#013foo`.
    #[test]
    fn a_control_byte_in_the_cause_line_is_escaped_for_the_terminal() {
        let refusal = rsync_error!(
            1,
            "invalid modifier '\u{b}' at position 1 in filter rule: +\u{b}foo"
        );
        let (text, _) = rendered(Role::Client, &refusal);
        assert_eq!(
            text.lines().next(),
            Some("invalid modifier '\\#013' at position 1 in filter rule: +\\#013foo")
        );
    }
}

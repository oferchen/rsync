//! The two lines a fatal filter refusal prints before the process exits.

use std::io::{self, Write};

use crate::branding::Brand;
use crate::message::strings::exit_code_message;
use crate::message::{Message, Role};

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
/// # Errors
///
/// Any error writing to `out`.
pub fn write_refusal<W: Write>(
    out: &mut W,
    brand: Brand,
    who: Role,
    refusal: &Message,
) -> io::Result<i32> {
    let code = refusal.code().unwrap_or(1);
    if refusal.role().is_some() {
        writeln!(
            out,
            "{}: [{}] {}",
            brand.client_program_name(),
            who.as_str(),
            refusal.text()
        )?;
    } else {
        writeln!(out, "{}", refusal.text())?;
    }
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

    fn rendered(who: Role, refusal: &Message) -> (String, i32) {
        let mut out = Vec::new();
        let code = write_refusal(&mut out, Brand::Upstream, who, refusal).expect("write");
        (String::from_utf8(out).expect("utf8"), code)
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
}

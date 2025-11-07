use std::io::{self, Write};

use oc_rsync_core::client::ModuleList;

pub(crate) fn render_module_list<W: Write, E: Write>(
    stdout: &mut W,
    stderr: &mut E,
    list: &ModuleList,
    suppress_motd: bool,
) -> io::Result<()> {
    for warning in list.warnings() {
        writeln!(stderr, "@WARNING: {warning}")?;
    }

    if !suppress_motd {
        for line in list.motd_lines() {
            writeln!(stdout, "{line}")?;
        }
    }

    for entry in list.entries() {
        let name = entry.name();
        if let Some(comment) = entry.comment() {
            writeln!(stdout, "{name}\t{comment}")?;
        } else {
            writeln!(stdout, "{name}")?;
        }
    }
    Ok(())
}

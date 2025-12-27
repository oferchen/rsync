use std::io::{self, Write};

use core::client::ModuleList;

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

#[cfg(test)]
mod tests {
    // Note: ModuleList requires complex construction and is typically created
    // from daemon responses. The render_module_list function is tested
    // through integration tests that perform actual daemon connections.
    // This module has minimal unit testing since the function signature
    // and implementation are straightforward write operations.

    #[test]
    fn module_compiles() {
        // This test ensures the module compiles correctly
    }
}

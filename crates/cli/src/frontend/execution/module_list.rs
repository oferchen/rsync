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

    // upstream: clientserver.c:1254 - `io_printf(fd, "%-15s\t%s\n", name, comment)`
    // always emits the name, a tab, then the (possibly empty) comment. The name
    // already carries its `%-15s` padding (preserved by the `\t` split in
    // ModuleListEntry::from_line), so a comment-less module renders as
    // `name<pad>\t` - matching upstream rather than dropping the trailing tab.
    for entry in list.entries() {
        let name = entry.name();
        let comment = entry.comment().unwrap_or("");
        writeln!(stdout, "{name}\t{comment}")?;
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

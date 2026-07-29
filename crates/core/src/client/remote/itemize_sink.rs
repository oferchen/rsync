//! Client-visible itemize sink shared by the remote transports.
//!
//! On an SSH or daemon transfer the client's `--out-format` / `-v` / `-i` rows
//! surface through a [`crate::server::ItemizeCallback`]. This sink routes each
//! row to one of two destinations depending on whether a custom `--out-format`
//! template is active.

use std::io::Write;

use super::super::summary::{ClientEvent, RemoteItemizeFields};

/// Routes a transport's client-visible itemize rows to the right destination.
///
/// With a custom `--out-format` active (`collect`), each row is captured as a
/// metadata-bearing [`ClientEvent`] so the CLI renders it through the same
/// out-format path as a local transfer. Otherwise the server's pre-formatted
/// line is written straight to stdout, preserving the default `-v`/`-i` output
/// byte-for-byte (upstream `log_item(FCLIENT, ...)`).
pub(crate) struct ItemizeEventSink {
    collect: bool,
    events: Vec<ClientEvent>,
}

impl ItemizeEventSink {
    /// Creates a sink that collects events when `collect` is set, otherwise
    /// writes each row's pre-formatted line to stdout.
    pub(crate) const fn new(collect: bool) -> Self {
        Self {
            collect,
            events: Vec::new(),
        }
    }

    /// Takes the collected events, leaving the sink empty.
    pub(crate) fn take_events(&mut self) -> Vec<ClientEvent> {
        std::mem::take(&mut self.events)
    }

    fn write_line(line: &str) {
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(line.as_bytes());
    }
}

impl crate::server::ItemizeCallback for ItemizeEventSink {
    fn on_itemize(&mut self, line: &str) {
        Self::write_line(line);
    }

    fn on_itemize_row(&mut self, row: &crate::server::ItemizeRow<'_>) {
        if self.collect {
            self.events
                .push(ClientEvent::from_remote_itemize(RemoteItemizeFields {
                    relative_path: row.name.to_path_buf(),
                    source_prefix: row.source_prefix.map(std::path::Path::to_path_buf),
                    itemize: row.itemize.to_owned(),
                    mode: row.mode,
                    size: row.size,
                    mtime: row.mtime,
                    mtime_nsec: row.mtime_nsec,
                    uid: row.uid,
                    gid: row.gid,
                    is_dir: row.is_dir,
                    is_symlink: row.is_symlink,
                    symlink_target: row.symlink_target.map(std::path::Path::to_path_buf),
                    hardlink_leader: row.hardlink_leader.map(std::path::Path::to_path_buf),
                    is_new: row.is_new,
                    is_deletion: row.is_deletion,
                }));
        } else {
            Self::write_line(row.line);
        }
    }
}

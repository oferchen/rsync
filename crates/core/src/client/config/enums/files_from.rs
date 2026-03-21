/// Describes the source for `--files-from` file list entries.
///
/// In upstream rsync, `--files-from` can specify a local file, stdin, or a
/// remote file (via the `:path` or `host:path` syntax). When the file is
/// remote, the server opens it directly; when local, the client reads it
/// and forwards content over the protocol.
///
/// # Upstream Reference
///
/// - `options.c:2447-2490` - files_from parsing and filesfrom_host/fd setup
/// - `options.c:2944-2956` - server_options() forwarding to remote server
#[derive(Clone, Debug, Eq, PartialEq, Default)]
pub enum FilesFromSource {
    /// No `--files-from` specified.
    #[default]
    None,
    /// Local file path (read by the client, forwarded over the protocol).
    LocalFile(std::path::PathBuf),
    /// Read from standard input (`--files-from=-` on the client side).
    Stdin,
    /// Remote file path (read directly by the server).
    ///
    /// The colon prefix (`:`) has been stripped. For SSH transfers, the host
    /// is taken from the transfer operand, not from the `--files-from` value.
    ///
    /// # Upstream Reference
    ///
    /// - `options.c:2458` - `check_for_hostspec()` detects `:path` prefix
    RemoteFile(String),
}

impl FilesFromSource {
    /// Returns `true` when a `--files-from` source has been configured.
    #[must_use]
    pub fn is_active(&self) -> bool {
        !matches!(self, Self::None)
    }

    /// Returns `true` when the file list is read on the remote server.
    #[must_use]
    pub fn is_remote(&self) -> bool {
        matches!(self, Self::RemoteFile(_))
    }

    /// Returns `true` when the file list is read locally and must be
    /// forwarded over the protocol to the sender.
    #[must_use]
    pub fn is_local_forwarded(&self) -> bool {
        matches!(self, Self::LocalFile(_) | Self::Stdin)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn default_is_none() {
        assert_eq!(FilesFromSource::default(), FilesFromSource::None);
    }

    #[test]
    fn is_active_none() {
        assert!(!FilesFromSource::None.is_active());
    }

    #[test]
    fn is_active_local_file() {
        assert!(FilesFromSource::LocalFile(PathBuf::from("/tmp/list")).is_active());
    }

    #[test]
    fn is_active_stdin() {
        assert!(FilesFromSource::Stdin.is_active());
    }

    #[test]
    fn is_active_remote() {
        assert!(FilesFromSource::RemoteFile("/remote/list".to_owned()).is_active());
    }

    #[test]
    fn is_remote_false_for_none() {
        assert!(!FilesFromSource::None.is_remote());
    }

    #[test]
    fn is_remote_false_for_local() {
        assert!(!FilesFromSource::LocalFile(PathBuf::from("/tmp")).is_remote());
    }

    #[test]
    fn is_remote_false_for_stdin() {
        assert!(!FilesFromSource::Stdin.is_remote());
    }

    #[test]
    fn is_remote_true_for_remote() {
        assert!(FilesFromSource::RemoteFile("/path".to_owned()).is_remote());
    }

    #[test]
    fn is_local_forwarded_false_for_none() {
        assert!(!FilesFromSource::None.is_local_forwarded());
    }

    #[test]
    fn is_local_forwarded_true_for_local_file() {
        assert!(FilesFromSource::LocalFile(PathBuf::from("/tmp")).is_local_forwarded());
    }

    #[test]
    fn is_local_forwarded_true_for_stdin() {
        assert!(FilesFromSource::Stdin.is_local_forwarded());
    }

    #[test]
    fn is_local_forwarded_false_for_remote() {
        assert!(!FilesFromSource::RemoteFile("/path".to_owned()).is_local_forwarded());
    }

    #[test]
    fn clone_eq() {
        let source = FilesFromSource::RemoteFile("/path".to_owned());
        let cloned = source.clone();
        assert_eq!(source, cloned);
    }

    #[test]
    fn debug_format() {
        assert!(format!("{:?}", FilesFromSource::None).contains("None"));
        assert!(format!("{:?}", FilesFromSource::Stdin).contains("Stdin"));
    }
}

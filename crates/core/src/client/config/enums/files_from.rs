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
    /// A `localhost:path` hostspec whose stripped path is openable locally.
    ///
    /// Upstream rsync keeps a single files-from fd: it either opens the file
    /// locally or reads it from the wire fd, never both
    /// (`options.c:2476-2501`). This variant defers that choice to
    /// [`FilesFromSource::resolve_for`], which collapses it into a plain local
    /// open in whichever transfer direction applies:
    ///
    /// - PUSH: the local sender opens `local_path` directly (like a plain
    ///   local file); `wire_arg` is not forwarded.
    /// - PULL: the local receiver stages `local_path`'s bytes and the remote
    ///   sender reads `--files-from=-`; `wire_arg` is not forwarded.
    ///
    /// Matches upstream `options.c:3112-3138 check_for_hostspec` +
    /// `options.c:2476-2483` single-host semantics.
    HybridLocalRemote {
        /// Local filesystem path to open in the applicable direction.
        local_path: std::path::PathBuf,
        /// Stripped path from the hostspec. Retained for diagnostics and
        /// operand classification; never forwarded to the remote peer.
        wire_arg: String,
    },
}

/// Direction-resolved `--files-from` wiring for a single transfer.
///
/// Upstream rsync uses a single files-from file descriptor: a remote
/// `--files-from` is EITHER opened locally OR read from the live wire fd
/// (`filesfrom_fd = f_in`), never both (`options.c:2476-2501`,
/// `main.c:1322-1328`). [`FilesFromSource::resolve_for`] collapses the
/// hybrid local/remote variant into one of those two single-fd modes based
/// on transfer direction, so callers never stage local bytes AND forward a
/// wire arg for the same source.
#[derive(Clone, Debug, Eq, PartialEq, Default)]
pub struct FilesFromPlan {
    /// `files_from_path` for the local sender's generator config (PUSH side).
    ///
    /// `Some("-")` means read from stdin / forwarded wire bytes; `Some(path)`
    /// means open a local file; `None` means the local side is not the sender
    /// for this source.
    pub sender_files_from_path: Option<String>,
    /// `from0` paired with [`Self::sender_files_from_path`].
    pub sender_from0: bool,
    /// The `--files-from` argument to forward to the remote peer's argv.
    ///
    /// `None` omits the argument entirely (the remote side does not read the
    /// list). Wire-forwarded lists carry `"-"`; a remote-hosted file carries
    /// its path.
    pub remote_arg: Option<String>,
    /// `from0` to forward alongside [`Self::remote_arg`].
    pub remote_from0: bool,
    /// Whether the local receiver must stage the list bytes into
    /// `files_from_data` for forwarding to the remote sender (PULL only).
    pub stage_local_bytes: bool,
}

impl FilesFromSource {
    /// Returns `true` when a `--files-from` source has been configured.
    #[must_use]
    pub fn is_active(&self) -> bool {
        !matches!(self, Self::None)
    }

    /// Resolves this source into a direction-aware single-fd [`FilesFromPlan`].
    ///
    /// `is_push` is `true` when the local process is the sender (PUSH) and
    /// `false` when it is the receiver (PULL). `from0` is the client's
    /// `--from0` setting, applied to the local-file / remote-file fds that
    /// honour it; wire-forwarded `"-"` fds always force NUL termination to
    /// match upstream's `start_filesfrom_forwarding` framing.
    ///
    /// The [`Self::HybridLocalRemote`] variant - a `localhost:path` hostspec
    /// whose stripped path is openable locally - is the only variant whose
    /// behaviour depends on direction. Upstream never opens such a file AND
    /// forwards it on the wire; it picks one fd:
    ///
    /// - PUSH: the local sender opens `local_path` directly, exactly like a
    ///   plain [`Self::LocalFile`]. The remote receiver gets no `--files-from`
    ///   and no bytes are staged.
    /// - PULL: the local receiver stages `local_path`'s bytes and forwards
    ///   them; the remote sender reads `--files-from=-`, exactly like a plain
    ///   [`Self::LocalFile`]. The stripped `wire_arg` is never handed to the
    ///   remote sender (that would be the discarded second fd).
    ///
    /// All other variants resolve to their existing upstream behaviour.
    ///
    /// # Upstream Reference
    ///
    /// - `options.c:2476-2501` - single `filesfrom_fd` (local open or wire fd)
    /// - `main.c:1322-1328` - `filesfrom_fd = f_in` for remote files-from
    /// - `options.c:2962` - `server_options()` forwards the arg only when
    ///   `!am_sender || filesfrom_host`
    #[must_use]
    pub fn resolve_for(&self, is_push: bool, from0: bool) -> FilesFromPlan {
        match self {
            Self::None => FilesFromPlan::default(),
            Self::Stdin => {
                if is_push {
                    FilesFromPlan {
                        sender_files_from_path: Some("-".to_owned()),
                        sender_from0: from0,
                        ..FilesFromPlan::default()
                    }
                } else {
                    FilesFromPlan {
                        remote_arg: Some("-".to_owned()),
                        remote_from0: true,
                        stage_local_bytes: true,
                        ..FilesFromPlan::default()
                    }
                }
            }
            Self::LocalFile(path) => {
                let local = path.to_string_lossy().into_owned();
                if is_push {
                    FilesFromPlan {
                        sender_files_from_path: Some(local),
                        sender_from0: from0,
                        ..FilesFromPlan::default()
                    }
                } else {
                    FilesFromPlan {
                        remote_arg: Some("-".to_owned()),
                        remote_from0: true,
                        stage_local_bytes: true,
                        ..FilesFromPlan::default()
                    }
                }
            }
            Self::RemoteFile(path) => {
                if is_push {
                    // Remote receiver opens the file and forwards its bytes
                    // back; the local sender reads them as `--files-from=-`.
                    // upstream: main.c:1191-1198 start_filesfrom_forwarding.
                    FilesFromPlan {
                        sender_files_from_path: Some("-".to_owned()),
                        sender_from0: true,
                        remote_arg: Some(path.clone()),
                        remote_from0: from0,
                        ..FilesFromPlan::default()
                    }
                } else {
                    // Remote sender opens the file directly via its argv.
                    FilesFromPlan {
                        remote_arg: Some(path.clone()),
                        remote_from0: from0,
                        ..FilesFromPlan::default()
                    }
                }
            }
            Self::HybridLocalRemote { local_path, .. } => {
                // Single-fd collapse: a localhost:path hostspec is treated as
                // a plain local file in whichever direction applies. The
                // stripped wire_arg is intentionally discarded so the remote
                // peer never opens a second fd for the same list.
                let local = local_path.to_string_lossy().into_owned();
                if is_push {
                    FilesFromPlan {
                        sender_files_from_path: Some(local),
                        sender_from0: from0,
                        ..FilesFromPlan::default()
                    }
                } else {
                    FilesFromPlan {
                        remote_arg: Some("-".to_owned()),
                        remote_from0: true,
                        stage_local_bytes: true,
                        ..FilesFromPlan::default()
                    }
                }
            }
        }
    }

    /// Returns `true` when the `--files-from` operand is a hostspec rather
    /// than a path the client opens as an operand file list.
    ///
    /// Used by operand classification to decide whether to load the file
    /// list locally. The hybrid variant counts as remote here because its
    /// operand is a `localhost:path` hostspec; its actual single-fd handling
    /// is resolved per direction by [`Self::resolve_for`].
    #[must_use]
    pub fn is_remote(&self) -> bool {
        matches!(self, Self::RemoteFile(_) | Self::HybridLocalRemote { .. })
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

    fn hybrid() -> FilesFromSource {
        FilesFromSource::HybridLocalRemote {
            local_path: PathBuf::from("/tmp/list"),
            wire_arg: "/tmp/list".to_owned(),
        }
    }

    #[test]
    fn resolve_none_is_empty_both_directions() {
        assert_eq!(
            FilesFromSource::None.resolve_for(true, false),
            FilesFromPlan::default()
        );
        assert_eq!(
            FilesFromSource::None.resolve_for(false, false),
            FilesFromPlan::default()
        );
    }

    // PUSH-Hybrid behaves as a plain local open: the sender opens local_path,
    // no wire arg is forwarded, and nothing is staged. This is the fix for
    // symptom A (duplicate >f+++++++++ from a double files-from source).
    #[test]
    fn resolve_hybrid_push_opens_local_path_only() {
        let plan = hybrid().resolve_for(true, false);
        assert_eq!(plan.sender_files_from_path.as_deref(), Some("/tmp/list"));
        assert!(!plan.sender_from0);
        assert_eq!(plan.remote_arg, None);
        assert!(!plan.stage_local_bytes);
    }

    #[test]
    fn resolve_hybrid_push_honours_from0() {
        let plan = hybrid().resolve_for(true, true);
        assert_eq!(plan.sender_files_from_path.as_deref(), Some("/tmp/list"));
        assert!(plan.sender_from0);
    }

    // PULL-Hybrid stages the local bytes and forwards `--files-from=-` to the
    // remote sender - never the stripped wire arg. This is the fix for
    // symptom B (NDX 101 vs 1 protocol violation from the discarded second fd).
    #[test]
    fn resolve_hybrid_pull_stages_bytes_and_forwards_dash() {
        let plan = hybrid().resolve_for(false, false);
        assert!(plan.stage_local_bytes);
        assert_eq!(plan.remote_arg.as_deref(), Some("-"));
        assert!(plan.remote_from0);
        assert_eq!(plan.sender_files_from_path, None);
    }

    #[test]
    fn resolve_local_file_push_opens_path() {
        let src = FilesFromSource::LocalFile(PathBuf::from("/tmp/list"));
        let plan = src.resolve_for(true, true);
        assert_eq!(plan.sender_files_from_path.as_deref(), Some("/tmp/list"));
        assert!(plan.sender_from0);
        assert!(!plan.stage_local_bytes);
        assert_eq!(plan.remote_arg, None);
    }

    #[test]
    fn resolve_local_file_pull_stages_and_forwards_dash() {
        let src = FilesFromSource::LocalFile(PathBuf::from("/tmp/list"));
        let plan = src.resolve_for(false, true);
        assert!(plan.stage_local_bytes);
        assert_eq!(plan.remote_arg.as_deref(), Some("-"));
        assert!(plan.remote_from0);
        assert_eq!(plan.sender_files_from_path, None);
    }

    #[test]
    fn resolve_stdin_push_reads_dash() {
        let plan = FilesFromSource::Stdin.resolve_for(true, false);
        assert_eq!(plan.sender_files_from_path.as_deref(), Some("-"));
        assert!(!plan.stage_local_bytes);
        assert_eq!(plan.remote_arg, None);
    }

    #[test]
    fn resolve_stdin_pull_stages_and_forwards_dash() {
        let plan = FilesFromSource::Stdin.resolve_for(false, false);
        assert!(plan.stage_local_bytes);
        assert_eq!(plan.remote_arg.as_deref(), Some("-"));
        assert!(plan.remote_from0);
    }

    #[test]
    fn resolve_remote_file_push_reads_wire_and_forwards_path() {
        let src = FilesFromSource::RemoteFile("/remote/list".to_owned());
        let plan = src.resolve_for(true, false);
        assert_eq!(plan.sender_files_from_path.as_deref(), Some("-"));
        assert!(plan.sender_from0);
        assert_eq!(plan.remote_arg.as_deref(), Some("/remote/list"));
        assert!(!plan.remote_from0);
        assert!(!plan.stage_local_bytes);
    }

    #[test]
    fn resolve_remote_file_pull_forwards_path_only() {
        let src = FilesFromSource::RemoteFile("/remote/list".to_owned());
        let plan = src.resolve_for(false, true);
        assert_eq!(plan.sender_files_from_path, None);
        assert_eq!(plan.remote_arg.as_deref(), Some("/remote/list"));
        assert!(plan.remote_from0);
        assert!(!plan.stage_local_bytes);
    }
}

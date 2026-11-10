// Filter options a client passes in the daemon's server argv.
//
// An upstream client never sends filter rules in the argv on its own; they
// travel on the filter list (`exclude.c:1944`). `-M` / `--remote-option`
// hands any option to the daemon's `parse_arguments()`, which applies these
// exactly as a command line would, so each one is a peer-chosen rule and the
// `-from` spellings are peer-chosen file opens.

/// The message upstream sends when a client argv filter option names a file
/// the daemon's filter hides.
///
/// upstream: `options.c:2684-2686`.
const ARGV_FILTER_OPTIONS_REJECTED: &str = "Your options have been rejected by the server.";

/// Why a client argv filter option ends the session.
#[derive(Debug)]
enum ArgvFilterRefusal {
    /// A `-from` file the daemon's filter hides, or an empty `-from` name.
    ///
    /// upstream: `options.c:1636-1652` jumps to `options_rejected`.
    Rejected,
    /// A malformed rule or an unreadable file, with upstream's exit code.
    ///
    /// upstream: `filter_rule_err()` exits `RERR_SYNTAX` (`exclude.c:132-136`);
    /// `XFLG_FATAL_ERRORS` makes a failed open exit `RERR_FILEIO`
    /// (`exclude.c:1714-1719`).
    Fatal {
        /// The exit code the session ends with.
        code: ExitCode,
        /// The cause, as the parser reported it.
        text: String,
    },
}

/// The client's options, read the way the server's popt pass reads them.
///
/// Only the tokens ahead of the `.` operand separator are options. They go
/// through the same `normalize_server_argv` as a remote-shell server's argv,
/// so a filter inside a short cluster (`-rf- x`), a split `--exclude x` and a
/// `-F` all arrive as joined `--opt=VALUE` tokens, those ahead of `--server`
/// twice (options.c:1517-1524). An argv that pass refuses is read as sent:
/// refusing it is not this reader's job.
fn normalized_client_options(client_args: &[String]) -> Vec<std::ffi::OsString> {
    let end = client_args
        .iter()
        .position(|arg| arg == ".")
        .unwrap_or(client_args.len());
    let options = &client_args[..end];
    // The rewrite takes an argv, whose first slot is the program name.
    let argv: Vec<std::ffi::OsString> = std::iter::once("rsyncd")
        .chain(options.iter().map(String::as_str))
        .map(std::ffi::OsString::from)
        .collect();
    match core::server::server_argv::normalize_server_argv(&argv) {
        Ok(normalized) => normalized.into_iter().skip(1).collect(),
        Err(_) => options.iter().map(std::ffi::OsString::from).collect(),
    }
}

/// The files a client's argv filter options name, as the daemon reads them.
///
/// upstream: a module without chroot sets `sanitize_paths`
/// (`clientserver.c:1067-1068`), so every name is sanitized to the module
/// root; a chrooted daemon reaches the same file through the chroot. A
/// `-from` name the module's rules hide rejects the options
/// (`options.c:1636-1652`); a hidden merge file is treated as absent, with no
/// open (`exclude.c:1639-1664`). Every other open is confined beneath the
/// module root.
struct ModuleArgvFiles<'a> {
    /// The module root every name resolves beneath.
    root: &'a Path,
    /// The module's own rules: upstream's `daemon_filter_list`.
    daemon_rules: &'a [FilterRuleWireFormat],
    /// Set when a hidden `-from` name rejected the options.
    rejected: std::cell::Cell<bool>,
}

impl core::client::filter_args::FilterFileSource for ModuleArgvFiles<'_> {
    fn base_dir(&self) -> std::path::PathBuf {
        self.root.to_path_buf()
    }

    fn open(
        &self,
        path: &Path,
        kind: core::client::filter_args::FilterFileKind,
    ) -> io::Result<Box<dyn std::io::Read>> {
        use core::client::filter_args::FilterFileKind;
        // A merge name arrives joined onto `base_dir`; a `-from` name as sent.
        let name = path.strip_prefix(self.root).unwrap_or(path);
        let relative = filters::sanitize_path::sanitize_path(&name.to_string_lossy());
        let hidden = !self.daemon_rules.is_empty()
            && ((kind == FilterFileKind::RuleList && relative.is_empty())
                || merge_file_hidden_by_daemon_rules(
                    self.daemon_rules,
                    Path::new(&relative),
                    self.root,
                ));
        if hidden {
            return match kind {
                FilterFileKind::MergeFile => Ok(Box::new(std::io::empty())),
                FilterFileKind::RuleList => {
                    self.rejected.set(true);
                    Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        ARGV_FILTER_OPTIONS_REJECTED,
                    ))
                }
            };
        }
        let content = read_module_file(self.root, &relative)?;
        Ok(Box::new(std::io::Cursor::new(content.into_bytes())))
    }
}

/// Builds the rules the client's argv filter options add, in argv order.
///
/// The options are parsed by the same builder a remote-shell server and the
/// client use; only where the files they name come from differs, through
/// [`ModuleArgvFiles`].
///
/// # Errors
///
/// [`ArgvFilterRefusal::Rejected`] for a hidden `-from` file and
/// [`ArgvFilterRefusal::Fatal`] for a malformed rule or an unreadable file.
///
/// # Upstream Reference
///
/// - `options.c:1620-1656` - the `--filter`, `--exclude`, `--include` and
///   `-from` arms of `parse_arguments()`.
fn build_argv_filter_rules(
    client_args: &[String],
    daemon_rules: &[FilterRuleWireFormat],
    module_root: &Path,
) -> Result<Vec<FilterRuleWireFormat>, ArgvFilterRefusal> {
    let options = normalized_client_options(client_args);
    let from0 = options.iter().any(|arg| arg == "--from0");
    let files = ModuleArgvFiles {
        root: module_root,
        daemon_rules,
        rejected: std::cell::Cell::new(false),
    };
    core::client::filter_args::server_argv_filter_rules(&options, from0, &files).map_err(
        |message| {
            if files.rejected.get() {
                ArgvFilterRefusal::Rejected
            } else {
                let code = if message.code() == Some(ExitCode::FileIo.as_i32()) {
                    ExitCode::FileIo
                } else {
                    ExitCode::Syntax
                };
                ArgvFilterRefusal::Fatal {
                    code,
                    text: message.text().to_owned(),
                }
            }
        },
    )
}

/// Reads `relative` beneath the module root, never leaving it.
///
/// On Unix the open goes through the confined resolver, so a symlink or `..`
/// cannot lead outside the module. Elsewhere the sanitized name, which can no
/// longer be absolute or climb, is joined onto the root.
fn read_module_file(root: &Path, relative: &str) -> io::Result<String> {
    #[cfg(unix)]
    {
        use std::io::Read as _;
        let mut file = fast_io::open_source_confined(
            root,
            Path::new(relative),
            fast_io::LeafPolicy::FollowConfined,
            false,
        )?;
        let mut content = String::new();
        file.read_to_string(&mut content)?;
        Ok(content)
    }
    #[cfg(not(unix))]
    {
        fs::read_to_string(root.join(relative))
    }
}

/// Ends the session over a client argv filter option.
///
/// A rejected `-from` name travels as a multiplexed error, the same way an
/// unparseable client argument does: `@RSYNCD: OK` is already on the wire.
/// A fatal error exits from inside upstream's `parse_arguments()`, before the
/// protocol setup that could carry it back, so the client sees only EOF and
/// the reason reaches the daemon log alone, followed by the totals line every
/// daemon exit logs.
///
/// upstream: `clientserver.c:1254-1267` reports the rejection's `err_buf` and
/// exits; `exclude.c:136` and `exclude.c:1714-1719` call `exit_cleanup()`
/// from `parse_arguments()` (`clientserver.c:1160`), ahead of the
/// `setup_protocol()` at `clientserver.c:1226`; `cleanup.c:222-226` runs
/// `log_exit()`, whose totals line is `log.c:942`.
fn refuse_argv_filters(
    ctx: &mut ModuleRequestContext<'_>,
    refusal: &ArgvFilterRefusal,
    protocol_version: Option<ProtocolVersion>,
    client_args: &[String],
) -> io::Result<()> {
    match refusal {
        ArgvFilterRefusal::Rejected => {
            if let Some(log) = ctx.log_sink {
                let text = format!("module '{}': {ARGV_FILTER_OPTIONS_REJECTED}", ctx.request);
                log_message(log, &rsync_warning!(text).with_role(Role::Daemon));
            }
            let error = AtError::message(ARGV_FILTER_OPTIONS_REJECTED);
            handle_client_arg_rejection_post_handshake(
                ctx,
                &error.line(),
                protocol_version,
                client_args,
            )
        }
        ArgvFilterRefusal::Fatal { code, text } => {
            let code = *code;
            *ctx.session_exit_code = Some(code);
            if let Some(log) = ctx.log_sink {
                log_message(
                    log,
                    &rsync_error!(code.as_i32(), text.clone()).with_role(Role::Daemon),
                );
                let totals = "sent 0 bytes  received 0 bytes  total size 0";
                log_message(log, &rsync_info!(totals).with_role(Role::Daemon));
            }
            Ok(())
        }
    }
}

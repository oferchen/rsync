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
/// upstream: `options.c:2739-2741`.
const ARGV_FILTER_OPTIONS_REJECTED: &str = "Your options have been rejected by the server.";

/// Why a client argv filter option ends the session.
#[derive(Debug)]
enum ArgvFilterRefusal {
    /// A `-from` file the daemon's filter hides, or an empty `-from` name.
    ///
    /// upstream: `options.c:1663-1679` jumps to `options_rejected`.
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
        refusal: Message,
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
    /// The daemon's `who_am_i()` while it parses the options.
    who: Role,
}

impl core::client::filter_args::FilterFileSource for ModuleArgvFiles<'_> {
    fn base_dir(&self) -> std::path::PathBuf {
        self.root.to_path_buf()
    }

    /// A daemon sanitizes the name before it joins it, because a module
    /// without chroot sets `sanitize_paths` (clientserver.c:1067-1068). An
    /// absolute name lands beneath the module root and a leading `..` is
    /// dropped, so a refusal names the module-prefixed path, as 3.5.2 prints
    /// it (MEASURED: `. /etc/hostname` -> `<module>/etc/hostname`).
    ///
    /// upstream: exclude.c:715-747 parse_merge_name(), util1.c:1235
    /// sanitize_path() with `rootdir = module_dir` for an absolute name.
    fn merge_path(&self, name: &Path, _base_dir: &Path) -> std::path::PathBuf {
        if is_bare_merge_name(name) {
            return name.to_path_buf();
        }
        let sanitized = filters::sanitize_path::sanitize_path(&name.to_string_lossy());
        filters::collapse_dot_dot_dirs(&self.root.join(sanitized))
    }

    fn who(&self) -> Role {
        self.who
    }

    fn open(
        &self,
        path: &Path,
        kind: core::client::filter_args::FilterFileKind,
        _list: &[core::client::FilterRuleSpec],
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
        // upstream: exclude.c:704-715 parse_merge_name() returns a name with
        // no slash unchanged, unsanitized, so `..` reaches the confined open
        // and is refused there (ELOOP, MEASURED on 3.5.2) while `.` opens the
        // module root itself.
        let open_name = if is_bare_merge_name(name) {
            name.to_string_lossy().into_owned()
        } else {
            relative
        };
        open_module_file(self.root, &open_name)
    }
}

/// Whether a client-named merge file opens as given rather than sanitized.
///
/// Upstream's bare-name rule keys on `/` alone; only on Unix is that the sole
/// separator, so elsewhere a name such as `..\x` could still climb and every
/// name keeps its sanitized form.
fn is_bare_merge_name(name: &Path) -> bool {
    cfg!(unix) && !name.as_os_str().as_encoded_bytes().contains(&b'/')
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
        who: argv_parse_role(client_args),
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
                    refusal: message,
                }
            }
        },
    )
}

/// Opens `relative` beneath the module root, never leaving it.
///
/// The parser reads the file, so a name that opens as a directory ends at its
/// first read like an empty file, as on the client (exclude.c:1764-1769).
///
/// On Unix the open is upstream's: the ownership walk from the module root,
/// judged against the module (exclude.c:1677-1690), so a `..` that climbs out
/// or a symlink that points out is refused with `ELOOP`. Elsewhere the
/// sanitized name, which can no longer be absolute or climb, is joined onto
/// the root.
fn open_module_file(root: &Path, relative: &str) -> io::Result<Box<dyn std::io::Read>> {
    #[cfg(unix)]
    {
        let file = fast_io::operator_open_read_confined_at(root, Path::new(relative))?;
        Ok(Box::new(file))
    }
    #[cfg(not(unix))]
    {
        Ok(Box::new(fs::File::open(root.join(relative))?))
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
/// upstream: `clientserver.c:1263-1276` reports the rejection's `err_buf` and
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
        ArgvFilterRefusal::Fatal { code, refusal } => {
            *ctx.session_exit_code = Some(SessionExit::Refused(*code));
            let brand = ctx
                .log_sink
                .and_then(|log| log.lock().ok().map(|sink| sink.brand()))
                .unwrap_or(Brand::Oc);
            // Raw: the log sink below escapes each line itself, as upstream
            // escapes a daemon line once, in logit() (log.c:143); escaping
            // here too would turn `\#013` into `\#134#013`.
            let mut lines = Vec::new();
            let _ = core::client::filter_args::write_refusal(
                &mut lines,
                brand,
                argv_parse_role(client_args),
                refusal,
                logging::escape::EscapeStyle::passthrough(),
            );
            // upstream: msgs2stderr keeps its default of 2 until after
            // parse_arguments() returns (options.c:103, clientserver.c:1164-1182),
            // so rwrite() puts these lines on the daemon's own stderr as well as
            // in its log; the client reads only EOF.
            let _ = io::stderr().write_all(&lines);
            if let Some(log) = ctx.log_sink {
                for line in lines
                    .split(|byte| *byte == b'\n')
                    .filter(|line| !line.is_empty())
                {
                    log_raw_line(log, line);
                }
                let totals = "sent 0 bytes  received 0 bytes  total size 0";
                log_message(log, &rsync_info!(totals).with_role(Role::Daemon));
            }
            Ok(())
        }
    }
}

/// The `who_am_i()` a daemon reports while it parses the client's options.
///
/// upstream: rsync.c:987-995 - `am_sender` is already set from `--sender` by
/// then, and a receiving daemon has not forked, so it is the pre-fork
/// `Receiver`.
fn argv_parse_role(client_args: &[String]) -> Role {
    if client_args.iter().any(|arg| arg == "--sender") {
        Role::Sender
    } else {
        Role::PreForkReceiver
    }
}

#[cfg(test)]
mod argv_parse_role_tests {
    use super::*;

    #[test]
    fn a_name_with_a_slash_is_never_bare() {
        assert!(!is_bare_merge_name(Path::new("sub/rules")));
        assert!(!is_bare_merge_name(Path::new("../rules")));
    }

    #[cfg(unix)]
    #[test]
    fn a_slashless_unix_name_opens_as_given() {
        assert!(is_bare_merge_name(Path::new("..")));
        assert!(is_bare_merge_name(Path::new("rules")));
    }

    /// A backslash separates on Windows, so `..\secret` must keep its
    /// sanitized form instead of climbing out of the module.
    #[cfg(windows)]
    #[test]
    fn a_windows_name_is_never_bare() {
        assert!(!is_bare_merge_name(Path::new(r"..\secret")));
        assert!(!is_bare_merge_name(Path::new("..")));
    }

    /// upstream: rsync.c:987-995 - a 3.5.2 daemon prints its argv filter
    /// refusal as `[sender]` on a pull and as the pre-fork `[Receiver]` on a
    /// push (measured with `-M--filter='. nope'`).
    #[test]
    fn the_refusal_names_the_daemons_side() {
        let pull = ["--server".to_owned(), "--sender".to_owned(), ".".to_owned()];
        let push = ["--server".to_owned(), ".".to_owned()];
        assert_eq!(argv_parse_role(&pull), Role::Sender);
        assert_eq!(argv_parse_role(&push), Role::PreForkReceiver);
    }
}

#[cfg(test)]
mod merge_path_tests {
    use super::*;
    use core::client::filter_args::FilterFileSource;

    fn files(root: &Path) -> ModuleArgvFiles<'_> {
        ModuleArgvFiles {
            root,
            daemon_rules: &[],
            rejected: std::cell::Cell::new(false),
            who: Role::Sender,
        }
    }

    /// upstream: exclude.c:715-747 - with `sanitize_paths` a slash-bearing
    /// merge name is sanitized and lands beneath the module, so 3.5.2 reports
    /// `. /etc/hostname` as `<module>/etc/hostname` and `. sub/../../x` as
    /// `<module>/x` (MEASURED); oc reported the names as given.
    #[test]
    fn a_slash_bearing_name_resolves_beneath_the_module() {
        let root = Path::new("/srv/module");
        let files = files(root);
        for (name, expected) in [
            ("/etc/hostname", "/srv/module/etc/hostname"),
            ("sub/../../outrules", "/srv/module/outrules"),
            ("a/./b", "/srv/module/a/b"),
            ("sub/..", "/srv/module"),
        ] {
            assert_eq!(
                files.merge_path(Path::new(name), root),
                Path::new(expected),
                "{name}"
            );
        }
    }

    /// A bare name stays as given, so `..` still reaches the confined open
    /// and is refused there (exclude.c:704-715).
    #[cfg(unix)]
    #[test]
    fn a_bare_name_is_left_as_given() {
        let root = Path::new("/srv/module");
        for name in ["..", "rules"] {
            assert_eq!(
                files(root).merge_path(Path::new(name), root),
                Path::new(name)
            );
        }
    }
}

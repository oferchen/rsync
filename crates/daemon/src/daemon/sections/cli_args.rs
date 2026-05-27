/// Identifies the invoked daemon binary, controlling branding and help text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProgramName {
    /// Upstream-compatible `rsyncd` branding.
    Rsyncd,
    /// OC-branded `oc-rsyncd` binary.
    OcRsyncd,
}

impl ProgramName {
    #[inline]
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Rsyncd => Brand::Upstream.daemon_program_name(),
            Self::OcRsyncd => Brand::Oc.daemon_program_name(),
        }
    }

    #[inline]
    pub(crate) const fn brand(self) -> Brand {
        match self {
            Self::Rsyncd => Brand::Upstream,
            Self::OcRsyncd => Brand::Oc,
        }
    }
}

fn detect_program_name(program: Option<&OsStr>) -> ProgramName {
    match branding::detect_brand(program) {
        Brand::Oc => ProgramName::OcRsyncd,
        Brand::Upstream => ProgramName::Rsyncd,
    }
}

/// Windows Service action requested via CLI flags.
///
/// These flags are available on all platforms but only functional on Windows.
/// On non-Windows platforms they return a graceful error.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ServiceAction {
    /// Run as a Windows Service (SCM-managed lifecycle).
    RunAsService,
    /// Register the service with the Windows SCM and exit.
    Install,
    /// Remove the service from the Windows SCM and exit.
    Uninstall,
}

/// Result of parsing the top-level daemon CLI arguments.
///
/// `show_help` and `show_version` are handled before the daemon loop starts.
/// `remainder` is forwarded to `RuntimeOptions` for full option parsing.
pub(crate) struct ParsedArgs {
    pub(crate) program_name: ProgramName,
    pub(crate) show_help: bool,
    pub(crate) show_version: bool,
    pub(crate) service_action: Option<ServiceAction>,
    pub(crate) remainder: Vec<OsString>,
    /// TLS configuration parsed from `--ssl*` flags.
    ///
    /// Present only when `--ssl` is given and the `daemon-tls` feature is
    /// enabled. Contains paths to the certificate chain, private key, and
    /// optional client CA bundle for mutual TLS.
    #[cfg(feature = "daemon-tls")]
    pub(crate) tls_config: Option<crate::tls::TlsConfig>,
}

/// Builds the clap [`Command`] used by [`parse_args`].
///
/// Only `--help` and `--version` are extracted here; all other flags are
/// collected as `remainder` and forwarded to the daemon option parser.
pub(crate) fn clap_command(program_name: &'static str) -> Command {
    #[allow(unused_mut)]
    let mut cmd = Command::new(program_name)
        .disable_help_flag(true)
        .disable_version_flag(true)
        .arg_required_else_help(false)
        .arg(
            Arg::new("help")
                .long("help")
                .help("Show this help message and exit.")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("version")
                .long("version")
                .short('V')
                .help("Output version information and exit.")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("windows-service")
                .long("windows-service")
                .help("Run as a Windows Service (SCM-managed lifecycle).")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("install-service")
                .long("install-service")
                .help("Register the daemon as a Windows Service and exit.")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("uninstall-service")
                .long("uninstall-service")
                .help("Remove the daemon Windows Service registration and exit.")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("args")
                .action(ArgAction::Append)
                .num_args(0..)
                .allow_hyphen_values(true)
                .trailing_var_arg(true)
                .value_parser(OsStringValueParser::new()),
        );

    #[cfg(feature = "daemon-tls")]
    {
        cmd = cmd
            .arg(
                Arg::new("ssl")
                    .long("ssl")
                    .help("Enable native TLS on the daemon listener.")
                    .action(ArgAction::SetTrue),
            )
            .arg(
                Arg::new("ssl-cert")
                    .long("ssl-cert")
                    .help("Path to PEM certificate chain for TLS.")
                    .value_name("PATH")
                    .requires("ssl"),
            )
            .arg(
                Arg::new("ssl-key")
                    .long("ssl-key")
                    .help("Path to PEM private key for TLS.")
                    .value_name("PATH")
                    .requires("ssl"),
            )
            .arg(
                Arg::new("ssl-ca")
                    .long("ssl-ca")
                    .help("Path to PEM CA bundle for mutual TLS client verification.")
                    .value_name("PATH")
                    .requires("ssl"),
            );
    }

    cmd
}

/// Parses the top-level daemon arguments, extracting `--help` and `--version`.
///
/// All unrecognised flags are captured in [`ParsedArgs::remainder`] for
/// downstream processing by `RuntimeOptions::parse_with_brand`.
///
/// # Errors
///
/// Returns a clap error if argument parsing fails.
pub(crate) fn parse_args<I, S>(arguments: I) -> Result<ParsedArgs, clap::Error>
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    let mut args: Vec<OsString> = arguments.into_iter().map(Into::into).collect();

    let program_name = detect_program_name(args.first().map(OsString::as_os_str));

    if args.is_empty() {
        args.push(OsString::from(program_name.as_str()));
    }

    let mut matches = clap_command(program_name.as_str()).try_get_matches_from(args)?;

    let show_help = matches.get_flag("help");
    let show_version = matches.get_flag("version");
    let windows_service = matches.get_flag("windows-service");
    let install_service = matches.get_flag("install-service");
    let uninstall_service = matches.get_flag("uninstall-service");
    let remainder = matches
        .remove_many::<OsString>("args")
        .map(|values| values.collect())
        .unwrap_or_default();

    let service_action = if install_service {
        Some(ServiceAction::Install)
    } else if uninstall_service {
        Some(ServiceAction::Uninstall)
    } else if windows_service {
        Some(ServiceAction::RunAsService)
    } else {
        None
    };

    #[cfg(feature = "daemon-tls")]
    let tls_config = if matches.get_flag("ssl") {
        Some(crate::tls::TlsConfig {
            cert_path: matches
                .get_one::<String>("ssl-cert")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("cert.pem")),
            key_path: matches
                .get_one::<String>("ssl-key")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("key.pem")),
            client_ca_path: matches.get_one::<String>("ssl-ca").map(PathBuf::from),
        })
    } else {
        None
    };

    Ok(ParsedArgs {
        program_name,
        show_help,
        show_version,
        service_action,
        remainder,
        #[cfg(feature = "daemon-tls")]
        tls_config,
    })
}

/// Returns the daemon help text for the given program name.
pub(crate) fn render_help(program_name: ProgramName) -> String {
    help_text(program_name.brand())
}


#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProgramName {
    Rsyncd,
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

/// Parsed command produced by [`parse_args`].
/// Parsed command produced by [`parse_args`].
pub(crate) struct ParsedArgs {
    pub(crate) program_name: ProgramName,
    pub(crate) show_help: bool,
    pub(crate) show_version: bool,
    pub(crate) delegate_system_rsync: bool,
    pub(crate) remainder: Vec<OsString>,
}

/// Parsed command produced by [`parse_args`].

fn clap_command(program_name: &'static str) -> Command {
    Command::new(program_name)
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
            Arg::new("delegate-system-rsync")
                .long("delegate-system-rsync")
                .help("Launch the system rsync daemon with the supplied arguments.")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("args")
                .action(ArgAction::Append)
                .num_args(0..)
                .allow_hyphen_values(true)
                .trailing_var_arg(true)
                .value_parser(OsStringValueParser::new()),
        )
}

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
    let delegate_system_rsync = matches.get_flag("delegate-system-rsync");
    let remainder = matches
        .remove_many::<OsString>("args")
        .map(|values| values.collect())
        .unwrap_or_default();

    Ok(ParsedArgs {
        program_name,
        show_help,
        show_version,
        delegate_system_rsync,
        remainder,
    })
}

pub(crate) fn render_help(program_name: ProgramName) -> String {
    help_text(program_name.brand())
}


use std::env;
use std::ffi::{OsStr, OsString};
use std::path::PathBuf;

use rsync_core::branding::{self, Brand};
use rsync_core::client::{AddressMode, DeleteMode, HumanReadableMode, StrongChecksumChoice};

use super::command_builder::clap_command;
use super::filter_rules::{collect_filter_arguments, locate_filter_arguments};
use super::progress::{NameOutputLevel, ProgressSetting};
use super::{parse_checksum_seed_argument, parse_human_readable_level};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProgramName {
    Rsync,
    OcRsync,
}

impl ProgramName {
    #[inline]
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Rsync => Brand::Upstream.client_program_name(),
            Self::OcRsync => Brand::Oc.client_program_name(),
        }
    }

    #[inline]
    pub(crate) const fn brand(self) -> Brand {
        match self {
            Self::Rsync => Brand::Upstream,
            Self::OcRsync => Brand::Oc,
        }
    }
}

pub(crate) fn detect_program_name(program: Option<&OsStr>) -> ProgramName {
    match branding::detect_brand(program) {
        Brand::Oc => ProgramName::OcRsync,
        Brand::Upstream => ProgramName::Rsync,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum BandwidthArgument {
    Limit(OsString),
    Disabled,
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ParsedArgs {
    pub(crate) program_name: ProgramName,
    pub(crate) show_help: bool,
    pub(crate) show_version: bool,
    pub(crate) human_readable: Option<HumanReadableMode>,
    pub(crate) dry_run: bool,
    pub(crate) list_only: bool,
    pub(crate) remote_shell: Option<OsString>,
    pub(crate) connect_program: Option<OsString>,
    pub(crate) remote_options: Vec<OsString>,
    pub(crate) rsync_path: Option<OsString>,
    pub(crate) protect_args: Option<bool>,
    pub(crate) address_mode: AddressMode,
    pub(crate) bind_address: Option<OsString>,
    pub(crate) archive: bool,
    pub(crate) delete_mode: DeleteMode,
    pub(crate) delete_excluded: bool,
    pub(crate) backup: bool,
    pub(crate) backup_dir: Option<OsString>,
    pub(crate) backup_suffix: Option<OsString>,
    pub(crate) checksum: bool,
    pub(crate) checksum_choice: Option<StrongChecksumChoice>,
    pub(crate) checksum_choice_arg: Option<OsString>,
    pub(crate) checksum_seed: Option<u32>,
    pub(crate) size_only: bool,
    pub(crate) ignore_existing: bool,
    pub(crate) ignore_missing_args: bool,
    pub(crate) update: bool,
    pub(crate) remainder: Vec<OsString>,
    pub(crate) bwlimit: Option<BandwidthArgument>,
    pub(crate) max_delete: Option<OsString>,
    pub(crate) min_size: Option<OsString>,
    pub(crate) max_size: Option<OsString>,
    pub(crate) modify_window: Option<OsString>,
    pub(crate) compress: bool,
    pub(crate) no_compress: bool,
    pub(crate) compress_level: Option<OsString>,
    pub(crate) skip_compress: Option<OsString>,
    pub(crate) owner: Option<bool>,
    pub(crate) group: Option<bool>,
    pub(crate) chown: Option<OsString>,
    pub(crate) chmod: Vec<OsString>,
    pub(crate) perms: Option<bool>,
    pub(crate) super_mode: Option<bool>,
    pub(crate) times: Option<bool>,
    pub(crate) omit_dir_times: Option<bool>,
    pub(crate) omit_link_times: Option<bool>,
    pub(crate) acls: Option<bool>,
    pub(crate) numeric_ids: Option<bool>,
    pub(crate) hard_links: Option<bool>,
    pub(crate) sparse: Option<bool>,
    pub(crate) copy_links: Option<bool>,
    pub(crate) copy_dirlinks: bool,
    pub(crate) copy_unsafe_links: Option<bool>,
    pub(crate) keep_dirlinks: Option<bool>,
    pub(crate) safe_links: bool,
    pub(crate) devices: Option<bool>,
    pub(crate) specials: Option<bool>,
    pub(crate) relative: Option<bool>,
    pub(crate) one_file_system: Option<bool>,
    pub(crate) implied_dirs: Option<bool>,
    pub(crate) mkpath: bool,
    pub(crate) prune_empty_dirs: Option<bool>,
    pub(crate) verbosity: u8,
    pub(crate) progress: ProgressSetting,
    pub(crate) name_level: NameOutputLevel,
    pub(crate) name_overridden: bool,
    pub(crate) stats: bool,
    pub(crate) partial: bool,
    pub(crate) preallocate: bool,
    pub(crate) delay_updates: bool,
    pub(crate) partial_dir: Option<PathBuf>,
    pub(crate) temp_dir: Option<PathBuf>,
    pub(crate) link_dests: Vec<PathBuf>,
    pub(crate) remove_source_files: bool,
    pub(crate) inplace: Option<bool>,
    pub(crate) append: Option<bool>,
    pub(crate) append_verify: bool,
    pub(crate) msgs_to_stderr: bool,
    pub(crate) itemize_changes: bool,
    pub(crate) whole_file: Option<bool>,
    pub(crate) excludes: Vec<OsString>,
    pub(crate) includes: Vec<OsString>,
    pub(crate) compare_destinations: Vec<OsString>,
    pub(crate) copy_destinations: Vec<OsString>,
    pub(crate) link_destinations: Vec<OsString>,
    pub(crate) exclude_from: Vec<OsString>,
    pub(crate) include_from: Vec<OsString>,
    pub(crate) filters: Vec<OsString>,
    pub(crate) cvs_exclude: bool,
    pub(crate) rsync_filter_shortcuts: u8,
    pub(crate) files_from: Vec<OsString>,
    pub(crate) from0: bool,
    pub(crate) info: Vec<OsString>,
    pub(crate) debug: Vec<OsString>,
    pub(crate) xattrs: Option<bool>,
    pub(crate) no_motd: bool,
    pub(crate) password_file: Option<OsString>,
    pub(crate) protocol: Option<OsString>,
    pub(crate) timeout: Option<OsString>,
    pub(crate) contimeout: Option<OsString>,
    pub(crate) out_format: Option<OsString>,
    pub(crate) daemon_port: Option<u16>,
}

pub(crate) fn env_protect_args_default() -> Option<bool> {
    let value = env::var_os("RSYNC_PROTECT_ARGS")?;
    if value.is_empty() {
        return Some(true);
    }

    let normalized = value.to_string_lossy();
    let trimmed = normalized.trim();

    if trimmed.is_empty() {
        Some(true)
    } else if trimmed.eq_ignore_ascii_case("0")
        || trimmed.eq_ignore_ascii_case("no")
        || trimmed.eq_ignore_ascii_case("false")
        || trimmed.eq_ignore_ascii_case("off")
    {
        Some(false)
    } else {
        Some(true)
    }
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

    let raw_args = args.clone();
    let (filter_indices, rsync_filter_indices) = locate_filter_arguments(&raw_args);
    let mut matches = clap_command(program_name.as_str()).try_get_matches_from(args)?;

    let show_help = matches.get_flag("help");
    let show_version = matches.get_flag("version");
    let mut human_readable = matches
        .remove_one::<OsString>("human-readable")
        .map(|value| parse_human_readable_level(value.as_os_str()))
        .transpose()?;
    if matches.get_flag("no-human-readable") {
        human_readable = Some(HumanReadableMode::Disabled);
    }
    let mut dry_run = matches.get_flag("dry-run");
    let list_only = matches.get_flag("list-only");
    let mkpath = matches.get_flag("mkpath");
    let prune_empty_dirs = if matches.get_flag("no-prune-empty-dirs") {
        Some(false)
    } else if matches.get_flag("prune-empty-dirs") {
        Some(true)
    } else {
        None
    };
    let omit_link_times = if matches.get_flag("no-omit-link-times") {
        Some(false)
    } else if matches.get_flag("omit-link-times") {
        Some(true)
    } else {
        None
    };
    if list_only {
        dry_run = true;
    }
    let remote_shell = matches
        .remove_one::<OsString>("rsh")
        .filter(|value| !value.is_empty())
        .or_else(|| env::var_os("RSYNC_RSH").filter(|value| !value.is_empty()));
    let rsync_path = matches
        .remove_one::<OsString>("rsync-path")
        .filter(|value| !value.is_empty());
    let connect_program = matches
        .remove_one::<OsString>("connect-program")
        .filter(|value| !value.is_empty());
    let daemon_port = matches.remove_one::<u16>("port");
    let remote_options = matches
        .remove_many::<OsString>("remote-option")
        .map(|values| values.collect())
        .unwrap_or_default();
    let protect_args = if matches.get_flag("no-protect-args") {
        Some(false)
    } else if matches.get_flag("protect-args") {
        Some(true)
    } else {
        env_protect_args_default()
    };
    let address_mode = if matches.get_flag("ipv4") {
        AddressMode::Ipv4
    } else if matches.get_flag("ipv6") {
        AddressMode::Ipv6
    } else {
        AddressMode::Default
    };
    let bind_address_raw = matches.remove_one::<OsString>("address");
    let archive = matches.get_flag("archive");
    let delete_flag = matches.get_flag("delete");
    let delete_before_flag = matches.get_flag("delete-before");
    let delete_during_flag = matches.get_flag("delete-during");
    let delete_delay_flag = matches.get_flag("delete-delay");
    let delete_after_flag = matches.get_flag("delete-after");
    let ignore_missing_args = matches.get_flag("ignore-missing-args");
    let delete_excluded = matches.get_flag("delete-excluded");
    let max_delete = matches.remove_one::<OsString>("max-delete");
    let min_size = matches.remove_one::<OsString>("min-size");
    let max_size = matches.remove_one::<OsString>("max-size");
    let modify_window = matches.remove_one::<OsString>("modify-window");

    let delete_mode_conflicts = [
        delete_before_flag,
        delete_during_flag,
        delete_delay_flag,
        delete_after_flag,
    ]
    .into_iter()
    .filter(|flag| *flag)
    .count();

    if delete_mode_conflicts > 1 {
        return Err(clap::Error::raw(
            clap::error::ErrorKind::ArgumentConflict,
            "--delete-before, --delete-during, --delete-delay, and --delete-after are mutually exclusive",
        ));
    }

    let mut delete_mode = if delete_before_flag {
        DeleteMode::Before
    } else if delete_delay_flag {
        DeleteMode::Delay
    } else if delete_after_flag {
        DeleteMode::After
    } else if delete_during_flag || delete_flag {
        DeleteMode::During
    } else {
        DeleteMode::Disabled
    };

    if delete_excluded && !delete_mode.is_enabled() {
        delete_mode = DeleteMode::During;
    }
    if max_delete.is_some() && !delete_mode.is_enabled() {
        delete_mode = DeleteMode::During;
    }
    let mut backup = matches.get_flag("backup");
    let backup_dir = matches.remove_one::<OsString>("backup-dir");
    let backup_suffix = matches.remove_one::<OsString>("suffix");
    if backup_dir.is_some() || backup_suffix.is_some() {
        backup = true;
    }
    let compress_flag = matches.get_flag("compress");
    let no_compress = matches.get_flag("no-compress");
    let mut compress = if no_compress { false } else { compress_flag };
    let compress_level_opt = matches.get_one::<OsString>("compress-level").cloned();
    if let Some(ref value) = compress_level_opt {
        if let Ok(setting) = super::parse_compress_level_argument(value.as_os_str()) {
            compress = !setting.is_disabled();
        }
    }
    let owner = if matches.get_flag("owner") {
        Some(true)
    } else if matches.get_flag("no-owner") {
        Some(false)
    } else {
        None
    };
    let group = if matches.get_flag("group") {
        Some(true)
    } else if matches.get_flag("no-group") {
        Some(false)
    } else {
        None
    };
    let chown = matches.remove_one::<OsString>("chown");
    let chmod = matches
        .remove_many::<OsString>("chmod")
        .map(|values| values.collect())
        .unwrap_or_default();
    let perms = if matches.get_flag("perms") {
        Some(true)
    } else if matches.get_flag("no-perms") {
        Some(false)
    } else {
        None
    };
    let super_mode = if matches.get_flag("super") {
        Some(true)
    } else if matches.get_flag("no-super") {
        Some(false)
    } else {
        None
    };
    let times = if matches.get_flag("times") {
        Some(true)
    } else if matches.get_flag("no-times") {
        Some(false)
    } else {
        None
    };
    let omit_dir_times = if matches.get_flag("omit-dir-times") {
        Some(true)
    } else if matches.get_flag("no-omit-dir-times") {
        Some(false)
    } else {
        None
    };
    let acls = if matches.get_flag("acls") {
        Some(true)
    } else if matches.get_flag("no-acls") {
        Some(false)
    } else {
        None
    };
    let xattrs = if matches.get_flag("xattrs") {
        Some(true)
    } else if matches.get_flag("no-xattrs") {
        Some(false)
    } else {
        None
    };
    let numeric_ids = if matches.get_flag("numeric-ids") {
        Some(true)
    } else if matches.get_flag("no-numeric-ids") {
        Some(false)
    } else {
        None
    };
    let hard_links = if matches.get_flag("hard-links") {
        Some(true)
    } else if matches.get_flag("no-hard-links") {
        Some(false)
    } else {
        None
    };
    let sparse = if matches.get_flag("sparse") {
        Some(true)
    } else if matches.get_flag("no-sparse") {
        Some(false)
    } else {
        None
    };
    let copy_links = if matches.get_flag("copy-links") {
        Some(true)
    } else if matches.get_flag("no-copy-links") {
        Some(false)
    } else {
        None
    };
    let copy_dirlinks = matches.get_flag("copy-dirlinks");
    let copy_unsafe_links_option = if matches.get_flag("copy-unsafe-links") {
        Some(true)
    } else if matches.get_flag("no-copy-unsafe-links") || matches.get_flag("safe-links") {
        Some(false)
    } else {
        None
    };
    let keep_dirlinks = if matches.get_flag("keep-dirlinks") {
        Some(true)
    } else if matches.get_flag("no-keep-dirlinks") {
        Some(false)
    } else {
        None
    };
    let safe_links = matches.get_flag("safe-links") || copy_unsafe_links_option == Some(true);
    let devices = if matches.get_flag("devices") || matches.get_flag("archive-devices") {
        Some(true)
    } else if matches.get_flag("no-devices") {
        Some(false)
    } else {
        None
    };
    let specials = if matches.get_flag("specials") || matches.get_flag("archive-devices") {
        Some(true)
    } else if matches.get_flag("no-specials") {
        Some(false)
    } else {
        None
    };
    let relative = if matches.get_flag("relative") {
        Some(true)
    } else if matches.get_flag("no-relative") {
        Some(false)
    } else {
        None
    };
    let one_file_system = if matches.get_flag("one-file-system") {
        Some(true)
    } else if matches.get_flag("no-one-file-system") {
        Some(false)
    } else {
        None
    };
    let implied_dirs = if matches.get_flag("implied-dirs") {
        Some(true)
    } else if matches.get_flag("no-implied-dirs") {
        Some(false)
    } else {
        None
    };
    let msgs_to_stderr = matches.get_flag("msgs2stderr");
    let stats = matches.get_flag("stats");
    let partial_flag = matches.get_flag("partial") || matches.get_count("partial-progress") > 0;
    let preallocate = matches.get_flag("preallocate");
    let delay_updates = matches.get_flag("delay-updates") && !matches.get_flag("no-delay-updates");
    let partial_dir = matches
        .remove_one::<OsString>("partial-dir")
        .map(PathBuf::from);
    let partial = partial_flag || partial_dir.is_some();
    let temp_dir = matches
        .remove_one::<OsString>("temp-dir")
        .map(PathBuf::from);
    let link_dest_args: Vec<OsString> = matches
        .remove_many::<OsString>("link-dest")
        .map(|values| values.collect())
        .unwrap_or_default();
    let link_dests = link_dest_args
        .iter()
        .map(|value| PathBuf::from(value))
        .collect();
    let link_destinations = link_dest_args;
    let remove_source_files =
        matches.get_flag("remove-source-files") || matches.get_flag("remove-sent-files");
    let inplace = if matches.get_flag("inplace") {
        Some(true)
    } else if matches.get_flag("no-inplace") {
        Some(false)
    } else {
        None
    };
    let append_verify_flag = matches.get_flag("append-verify");
    let append = if append_verify_flag || matches.get_flag("append") {
        Some(true)
    } else if matches.get_flag("no-append") {
        Some(false)
    } else {
        None
    };
    let whole_file = if matches.get_flag("whole-file") {
        Some(true)
    } else if matches.get_flag("no-whole-file") {
        Some(false)
    } else {
        None
    };
    let progress_setting = if matches.get_flag("progress") {
        ProgressSetting::PerFile
    } else if matches.get_flag("no-progress") {
        ProgressSetting::Disabled
    } else {
        ProgressSetting::Unspecified
    };
    let name_level = if matches.get_flag("itemize-changes") {
        NameOutputLevel::UpdatedOnly
    } else {
        NameOutputLevel::Disabled
    };
    let name_overridden = matches.get_flag("itemize-changes");
    let mut verbosity = matches.get_count("verbose") as u8;
    if matches.get_flag("quiet") {
        verbosity = 0;
    }
    let remainder = matches
        .remove_many::<OsString>("args")
        .map(|values| values.collect())
        .unwrap_or_default();
    let checksum = matches.get_flag("checksum");
    let size_only = matches.get_flag("size-only");
    let (checksum_choice, checksum_choice_arg) =
        match matches.remove_one::<OsString>("checksum-choice") {
            Some(value) => {
                let text = value.to_string_lossy().into_owned();
                match StrongChecksumChoice::parse(&text) {
                    Ok(choice) => {
                        let normalized = OsString::from(choice.to_argument());
                        (Some(choice), Some(normalized))
                    }
                    Err(message) => {
                        return Err(clap::Error::raw(
                            clap::error::ErrorKind::ValueValidation,
                            message.text().to_string(),
                        ));
                    }
                }
            }
            None => (None, None),
        };

    let checksum_seed = match matches.remove_one::<OsString>("checksum-seed") {
        Some(value) => match parse_checksum_seed_argument(value.as_os_str()) {
            Ok(seed) => Some(seed),
            Err(message) => {
                return Err(clap::Error::raw(
                    clap::error::ErrorKind::ValueValidation,
                    message.text().to_string(),
                ));
            }
        },
        None => None,
    };

    let compress_level = matches.remove_one::<OsString>("compress-level");
    let skip_compress = matches.remove_one::<OsString>("skip-compress");
    let no_bwlimit = matches.get_flag("no-bwlimit");
    let bwlimit = if no_bwlimit {
        Some(BandwidthArgument::Disabled)
    } else {
        matches
            .remove_one::<OsString>("bwlimit")
            .map(BandwidthArgument::Limit)
    };
    let excludes = matches
        .remove_many::<OsString>("exclude")
        .map(|values| values.collect())
        .unwrap_or_default();
    let includes = matches
        .remove_many::<OsString>("include")
        .map(|values| values.collect())
        .unwrap_or_default();
    let compare_destinations = matches
        .remove_many::<OsString>("compare-dest")
        .map(|values| values.collect())
        .unwrap_or_default();
    let copy_destinations = matches
        .remove_many::<OsString>("copy-dest")
        .map(|values| values.collect())
        .unwrap_or_default();
    let exclude_from = matches
        .remove_many::<OsString>("exclude-from")
        .map(|values| values.collect())
        .unwrap_or_default();
    let include_from = matches
        .remove_many::<OsString>("include-from")
        .map(|values| values.collect())
        .unwrap_or_default();
    let filters: Vec<OsString> = matches
        .remove_many::<OsString>("filter")
        .map(|values| values.collect())
        .unwrap_or_default();
    let rsync_filter_shortcuts = rsync_filter_indices.len() as u8;
    let filter_args = collect_filter_arguments(&filters, &filter_indices, &rsync_filter_indices);
    let cvs_exclude = matches.get_flag("cvs-exclude");
    let files_from = matches
        .remove_many::<OsString>("files-from")
        .map(|values| values.collect())
        .unwrap_or_default();
    let from0 = matches.get_flag("from0");
    let info = matches
        .remove_many::<OsString>("info")
        .map(|values| values.collect())
        .unwrap_or_default();
    let debug = matches
        .remove_many::<OsString>("debug")
        .map(|values| values.collect())
        .unwrap_or_default();
    let ignore_existing = matches.get_flag("ignore-existing");
    let update = matches.get_flag("update");
    let password_file = matches.remove_one::<OsString>("password-file");
    let protocol = matches.remove_one::<OsString>("protocol");
    let timeout = matches.remove_one::<OsString>("timeout");
    let contimeout = matches.remove_one::<OsString>("contimeout");
    let out_format = matches.remove_one::<OsString>("out-format");
    let itemize_changes = matches.get_flag("itemize-changes");
    let no_motd = matches.get_flag("no-motd");

    Ok(ParsedArgs {
        program_name,
        show_help,
        show_version,
        human_readable,
        dry_run,
        list_only,
        remote_shell,
        connect_program,
        remote_options,
        rsync_path,
        protect_args,
        address_mode,
        bind_address: bind_address_raw,
        archive,
        delete_mode,
        delete_excluded,
        backup,
        backup_dir,
        backup_suffix,
        checksum,
        checksum_choice,
        checksum_choice_arg,
        checksum_seed,
        size_only,
        ignore_existing,
        ignore_missing_args,
        update,
        remainder,
        bwlimit,
        max_delete,
        min_size,
        max_size,
        modify_window,
        compress,
        no_compress,
        compress_level,
        skip_compress,
        owner,
        group,
        chown,
        chmod,
        perms,
        super_mode,
        times,
        omit_dir_times,
        omit_link_times,
        acls,
        numeric_ids,
        hard_links,
        sparse,
        copy_links,
        copy_dirlinks,
        copy_unsafe_links: copy_unsafe_links_option,
        keep_dirlinks,
        safe_links,
        devices,
        specials,
        relative,
        one_file_system,
        implied_dirs,
        mkpath,
        prune_empty_dirs,
        verbosity,
        progress: progress_setting,
        name_level,
        name_overridden,
        stats,
        partial,
        preallocate,
        delay_updates,
        partial_dir,
        temp_dir,
        link_dests,
        remove_source_files,
        inplace,
        append,
        append_verify: append_verify_flag,
        msgs_to_stderr,
        itemize_changes,
        whole_file,
        excludes,
        includes,
        compare_destinations,
        copy_destinations,
        link_destinations,
        exclude_from,
        include_from,
        filters: filter_args,
        cvs_exclude,
        rsync_filter_shortcuts,
        files_from,
        from0,
        info,
        debug,
        xattrs,
        no_motd,
        password_file,
        protocol,
        timeout,
        contimeout,
        out_format,
        daemon_port,
    })
}

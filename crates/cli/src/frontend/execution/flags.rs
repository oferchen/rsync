use std::ffi::OsString;

use core::{
    message::{Message, Role},
    rsync_error,
};

use super::super::progress::{NameOutputLevel, ProgressSetting};

#[derive(Default)]
pub(crate) struct InfoFlagSettings {
    pub(crate) progress: ProgressSetting,
    pub(crate) stats: Option<u8>,
    pub(crate) name: Option<NameOutputLevel>,
    pub(crate) backup: Option<u8>,
    pub(crate) copy: Option<u8>,
    pub(crate) del: Option<u8>,
    pub(crate) flist: Option<u8>,
    pub(crate) misc: Option<u8>,
    pub(crate) mount: Option<u8>,
    pub(crate) nonreg: Option<u8>,
    pub(crate) remove: Option<u8>,
    pub(crate) skip: Option<u8>,
    pub(crate) symsafe: Option<u8>,
    pub(crate) help_requested: bool,
}

impl InfoFlagSettings {
    const fn enable_all(&mut self) {
        self.progress = ProgressSetting::PerFile;
        self.stats = Some(1);
        self.name = Some(NameOutputLevel::UpdatedOnly);
        self.backup = Some(1);
        self.copy = Some(1);
        self.del = Some(1);
        self.flist = Some(1);
        self.misc = Some(1);
        self.mount = Some(1);
        self.nonreg = Some(1);
        self.remove = Some(1);
        self.skip = Some(1);
        self.symsafe = Some(1);
    }

    const fn disable_all(&mut self) {
        self.progress = ProgressSetting::Disabled;
        self.stats = Some(0);
        self.name = Some(NameOutputLevel::Disabled);
        self.backup = Some(0);
        self.copy = Some(0);
        self.del = Some(0);
        self.flist = Some(0);
        self.misc = Some(0);
        self.mount = Some(0);
        self.nonreg = Some(0);
        self.remove = Some(0);
        self.skip = Some(0);
        self.symsafe = Some(0);
    }

    fn apply(&mut self, token: &str, display: &str) -> Result<(), Message> {
        let lower = token.to_ascii_lowercase();

        if lower == "help" {
            self.help_requested = true;
            return Ok(());
        }

        if lower == "all" || lower == "1" {
            self.enable_all();
            return Ok(());
        }

        if lower == "none" || lower == "0" {
            self.disable_all();
            return Ok(());
        }

        let (normalized, level) = self.parse_flag_and_level(&lower);

        match normalized {
            "progress" => {
                self.progress = match level {
                    0 => ProgressSetting::Disabled,
                    1 => ProgressSetting::PerFile,
                    2 => ProgressSetting::Overall,
                    _ => return Err(info_flag_error(display)),
                };
                Ok(())
            }
            "stats" => {
                if level > 3 {
                    return Err(info_flag_error(display));
                }
                self.stats = Some(level);
                Ok(())
            }
            "name" => {
                let name_level = if level == 0 {
                    NameOutputLevel::Disabled
                } else if level == 1 {
                    NameOutputLevel::UpdatedOnly
                } else if level >= 2 {
                    NameOutputLevel::UpdatedAndUnchanged
                } else {
                    return Err(info_flag_error(display));
                };
                self.name = Some(name_level);
                Ok(())
            }
            "backup" => {
                self.backup = Some(level);
                Ok(())
            }
            "copy" => {
                self.copy = Some(level);
                Ok(())
            }
            "del" => {
                self.del = Some(level);
                Ok(())
            }
            "flist" => {
                if level > 2 {
                    return Err(info_flag_error(display));
                }
                self.flist = Some(level);
                Ok(())
            }
            "misc" => {
                if level > 2 {
                    return Err(info_flag_error(display));
                }
                self.misc = Some(level);
                Ok(())
            }
            "mount" => {
                self.mount = Some(level);
                Ok(())
            }
            "nonreg" => {
                self.nonreg = Some(level);
                Ok(())
            }
            "remove" => {
                self.remove = Some(level);
                Ok(())
            }
            "skip" => {
                if level > 2 {
                    return Err(info_flag_error(display));
                }
                self.skip = Some(level);
                Ok(())
            }
            "symsafe" => {
                self.symsafe = Some(level);
                Ok(())
            }
            _ => Err(info_flag_error(display)),
        }
    }

    /// Known info flag names for disambiguating `no-` prefix vs flag names
    /// that start with "no" (e.g., "nonreg").
    const KNOWN_FLAGS: &'static [&'static str] = &[
        "backup", "copy", "del", "flist", "misc", "mount", "name", "nonreg",
        "progress", "remove", "skip", "stats", "symsafe",
    ];

    fn parse_flag_and_level<'a>(&self, input: &'a str) -> (&'a str, u8) {
        // First try parsing as base+level (no negation prefix)
        let base = input.trim_end_matches(|c: char| c.is_ascii_digit());
        if base != input {
            // Has trailing digits -- check if the base is a known keyword
            if Self::KNOWN_FLAGS.contains(&base) {
                let suffix = &input[base.len()..];
                let level = suffix.parse::<u8>().unwrap_or(1);
                return (base, level);
            }
        } else if Self::KNOWN_FLAGS.contains(&input) {
            // Exact match with a known keyword, no digits
            return (input, 1);
        }

        // Try negation prefixes: "no" or "-"
        let stripped = input
            .strip_prefix("no")
            .or_else(|| input.strip_prefix('-'));

        if let Some(stripped) = stripped {
            return (stripped, 0);
        }

        // Fallback: no known keyword match, return as-is with level 1
        // (will be rejected by the caller's match statement)
        (input, 1)
    }
}

fn info_flag_error(display: &str) -> Message {
    rsync_error!(
        1,
        format!("invalid --info flag '{display}': use --info=help for supported flags")
    )
    .with_role(Role::Client)
}

pub(crate) fn parse_info_flags(values: &[OsString]) -> Result<InfoFlagSettings, Message> {
    let mut settings = InfoFlagSettings::default();
    for value in values {
        let text = value.to_string_lossy();
        let trimmed = text.trim_matches(|ch: char| ch.is_ascii_whitespace());

        if trimmed.is_empty() {
            return Err(rsync_error!(1, "--info flag must not be empty").with_role(Role::Client));
        }

        for token in trimmed.split(',') {
            let token = token.trim_matches(|ch: char| ch.is_ascii_whitespace());
            if token.is_empty() {
                return Err(
                    rsync_error!(1, "--info flag must not be empty").with_role(Role::Client)
                );
            }

            settings.apply(token, token)?;
        }
    }

    Ok(settings)
}

#[derive(Default)]
pub(crate) struct DebugFlagSettings {
    pub(crate) acl: Option<u8>,
    pub(crate) backup: Option<u8>,
    pub(crate) bind: Option<u8>,
    pub(crate) chdir: Option<u8>,
    pub(crate) connect: Option<u8>,
    pub(crate) cmd: Option<u8>,
    pub(crate) del: Option<u8>,
    pub(crate) deltasum: Option<u8>,
    pub(crate) dup: Option<u8>,
    pub(crate) exit: Option<u8>,
    pub(crate) filter: Option<u8>,
    pub(crate) flist: Option<u8>,
    pub(crate) fuzzy: Option<u8>,
    pub(crate) genr: Option<u8>,
    pub(crate) hash: Option<u8>,
    pub(crate) hlink: Option<u8>,
    pub(crate) iconv: Option<u8>,
    pub(crate) io: Option<u8>,
    pub(crate) nstr: Option<u8>,
    pub(crate) own: Option<u8>,
    pub(crate) proto: Option<u8>,
    pub(crate) recv: Option<u8>,
    pub(crate) send: Option<u8>,
    pub(crate) time: Option<u8>,
    pub(crate) help_requested: bool,
}

impl DebugFlagSettings {
    /// Returns an iterator over all flag (name, level) pairs that are set.
    pub(crate) fn iter_enabled_flags(&self) -> impl Iterator<Item = (&'static str, u8)> + '_ {
        [
            ("acl", self.acl),
            ("backup", self.backup),
            ("bind", self.bind),
            ("chdir", self.chdir),
            ("connect", self.connect),
            ("cmd", self.cmd),
            ("del", self.del),
            ("deltasum", self.deltasum),
            ("dup", self.dup),
            ("exit", self.exit),
            ("filter", self.filter),
            ("flist", self.flist),
            ("fuzzy", self.fuzzy),
            ("genr", self.genr),
            ("hash", self.hash),
            ("hlink", self.hlink),
            ("iconv", self.iconv),
            ("io", self.io),
            ("nstr", self.nstr),
            ("own", self.own),
            ("proto", self.proto),
            ("recv", self.recv),
            ("send", self.send),
            ("time", self.time),
        ]
        .into_iter()
        .filter_map(|(name, level)| level.filter(|&l| l > 0).map(|l| (name, l)))
    }

    const fn enable_all(&mut self) {
        self.acl = Some(1);
        self.backup = Some(1);
        self.bind = Some(1);
        self.chdir = Some(1);
        self.connect = Some(1);
        self.cmd = Some(1);
        self.del = Some(1);
        self.deltasum = Some(1);
        self.dup = Some(1);
        self.exit = Some(1);
        self.filter = Some(1);
        self.flist = Some(1);
        self.fuzzy = Some(1);
        self.genr = Some(1);
        self.hash = Some(1);
        self.hlink = Some(1);
        self.iconv = Some(1);
        self.io = Some(1);
        self.nstr = Some(1);
        self.own = Some(1);
        self.proto = Some(1);
        self.recv = Some(1);
        self.send = Some(1);
        self.time = Some(1);
    }

    const fn disable_all(&mut self) {
        self.acl = Some(0);
        self.backup = Some(0);
        self.bind = Some(0);
        self.chdir = Some(0);
        self.connect = Some(0);
        self.cmd = Some(0);
        self.del = Some(0);
        self.deltasum = Some(0);
        self.dup = Some(0);
        self.exit = Some(0);
        self.filter = Some(0);
        self.flist = Some(0);
        self.fuzzy = Some(0);
        self.genr = Some(0);
        self.hash = Some(0);
        self.hlink = Some(0);
        self.iconv = Some(0);
        self.io = Some(0);
        self.nstr = Some(0);
        self.own = Some(0);
        self.proto = Some(0);
        self.recv = Some(0);
        self.send = Some(0);
        self.time = Some(0);
    }

    fn apply(&mut self, token: &str, display: &str) -> Result<(), Message> {
        let lower = token.to_ascii_lowercase();

        if lower == "all" || lower == "1" {
            self.enable_all();
            return Ok(());
        }

        if lower == "none" || lower == "0" {
            self.disable_all();
            return Ok(());
        }

        let (normalized, level) = self.parse_flag_and_level(&lower);

        match normalized {
            "acl" => {
                self.acl = Some(level);
                Ok(())
            }
            "backup" => {
                if level > 2 {
                    return Err(debug_flag_error(display));
                }
                self.backup = Some(level);
                Ok(())
            }
            "bind" => {
                self.bind = Some(level);
                Ok(())
            }
            "chdir" => {
                self.chdir = Some(level);
                Ok(())
            }
            "connect" => {
                if level > 2 {
                    return Err(debug_flag_error(display));
                }
                self.connect = Some(level);
                Ok(())
            }
            "cmd" => {
                if level > 2 {
                    return Err(debug_flag_error(display));
                }
                self.cmd = Some(level);
                Ok(())
            }
            "del" => {
                if level > 3 {
                    return Err(debug_flag_error(display));
                }
                self.del = Some(level);
                Ok(())
            }
            "deltasum" => {
                if level > 4 {
                    return Err(debug_flag_error(display));
                }
                self.deltasum = Some(level);
                Ok(())
            }
            "dup" => {
                self.dup = Some(level);
                Ok(())
            }
            "exit" => {
                if level > 3 {
                    return Err(debug_flag_error(display));
                }
                self.exit = Some(level);
                Ok(())
            }
            "filter" => {
                if level > 3 {
                    return Err(debug_flag_error(display));
                }
                self.filter = Some(level);
                Ok(())
            }
            "flist" => {
                if level > 4 {
                    return Err(debug_flag_error(display));
                }
                self.flist = Some(level);
                Ok(())
            }
            "fuzzy" => {
                if level > 2 {
                    return Err(debug_flag_error(display));
                }
                self.fuzzy = Some(level);
                Ok(())
            }
            "genr" => {
                self.genr = Some(level);
                Ok(())
            }
            "hash" => {
                self.hash = Some(level);
                Ok(())
            }
            "hlink" => {
                if level > 3 {
                    return Err(debug_flag_error(display));
                }
                self.hlink = Some(level);
                Ok(())
            }
            "iconv" => {
                if level > 2 {
                    return Err(debug_flag_error(display));
                }
                self.iconv = Some(level);
                Ok(())
            }
            "io" => {
                if level > 4 {
                    return Err(debug_flag_error(display));
                }
                self.io = Some(level);
                Ok(())
            }
            "nstr" => {
                self.nstr = Some(level);
                Ok(())
            }
            "own" => {
                if level > 2 {
                    return Err(debug_flag_error(display));
                }
                self.own = Some(level);
                Ok(())
            }
            "proto" => {
                self.proto = Some(level);
                Ok(())
            }
            "recv" => {
                self.recv = Some(level);
                Ok(())
            }
            "send" => {
                self.send = Some(level);
                Ok(())
            }
            "time" => {
                if level > 2 {
                    return Err(debug_flag_error(display));
                }
                self.time = Some(level);
                Ok(())
            }
            _ => Err(debug_flag_error(display)),
        }
    }

    /// Known debug flag names for disambiguating `no-` prefix vs flag names
    /// that might start with "no".
    const KNOWN_FLAGS: &'static [&'static str] = &[
        "acl", "backup", "bind", "chdir", "connect", "cmd", "del",
        "deltasum", "dup", "exit", "filter", "flist", "fuzzy", "genr",
        "hash", "hlink", "iconv", "io", "nstr", "own", "proto", "recv",
        "send", "time",
    ];

    fn parse_flag_and_level<'a>(&self, input: &'a str) -> (&'a str, u8) {
        // First try parsing as base+level (no negation prefix)
        let base = input.trim_end_matches(|c: char| c.is_ascii_digit());
        if base != input {
            // Has trailing digits -- check if the base is a known keyword
            if Self::KNOWN_FLAGS.contains(&base) {
                let suffix = &input[base.len()..];
                let level = suffix.parse::<u8>().unwrap_or(1);
                return (base, level);
            }
        } else if Self::KNOWN_FLAGS.contains(&input) {
            // Exact match with a known keyword, no digits
            return (input, 1);
        }

        // Try negation prefixes: "no" or "-"
        let stripped = input
            .strip_prefix("no")
            .or_else(|| input.strip_prefix('-'));

        if let Some(stripped) = stripped {
            return (stripped, 0);
        }

        // Fallback: return as-is (will be rejected by the caller's match)
        (input, 1)
    }
}

pub(crate) fn parse_debug_flags(values: &[OsString]) -> Result<DebugFlagSettings, Message> {
    let mut settings = DebugFlagSettings::default();

    for value in values {
        let text = value.to_string_lossy();
        let trimmed = text.trim_matches(|ch: char| ch.is_ascii_whitespace());

        if trimmed.is_empty() {
            return Err(debug_flag_empty_error());
        }

        for token in trimmed.split(',') {
            let token = token.trim_matches(|ch: char| ch.is_ascii_whitespace());
            if token.is_empty() {
                return Err(debug_flag_empty_error());
            }

            if token.eq_ignore_ascii_case("help") {
                settings.help_requested = true;
            } else {
                settings.apply(token, token)?;
            }
        }
    }

    Ok(settings)
}

fn debug_flag_empty_error() -> Message {
    rsync_error!(1, "--debug flag must not be empty").with_role(Role::Client)
}

fn debug_flag_error(display: &str) -> Message {
    rsync_error!(
        1,
        format!("invalid --debug flag '{display}': use --debug=help for supported flags")
    )
    .with_role(Role::Client)
}

/// Check if progress-related flags are present in --info flags.
/// Used by fallback path to determine if progress output is enabled.
/// TODO: Will be used once fallback module is re-enabled
#[allow(dead_code)]
pub(crate) fn info_flags_include_progress(flags: &[OsString]) -> bool {
    flags.iter().any(|value| {
        value
            .to_string_lossy()
            .split(',')
            .map(|token| token.trim())
            .filter(|token| !token.is_empty())
            .any(|token| {
                let normalized = token.to_ascii_lowercase();
                let without_dash = normalized.strip_prefix('-').unwrap_or(&normalized);
                let stripped = without_dash
                    .strip_prefix("no-")
                    .or_else(|| without_dash.strip_prefix("no"))
                    .unwrap_or(without_dash);
                stripped.starts_with("progress")
            })
    })
}

pub(crate) const INFO_HELP_TEXT: &str = "The following --info flags are supported:\n\
    all         Enable all informational output currently implemented.\n\
    none        Disable all informational output handled by this build.\n\
    BACKUP      Mention files backed up.\n\
    COPY        Mention files copied locally on the receiving side.\n\
    DEL         Mention deletions on the receiving side.\n\
    FLIST       Mention file-list receiving/sending (levels 1-2).\n\
    MISC        Mention miscellaneous information (levels 1-2).\n\
    MOUNT       Mention mounts that were found or skipped.\n\
    NAME        Mention 1) updated file/dir names, 2) unchanged names.\n\
    NONREG      Mention skipped non-regular files (default 1, 0 disables).\n\
    PROGRESS    Mention 1) per-file progress or 2) total transfer progress.\n\
    REMOVE      Mention files removed on the sending side.\n\
    SKIP        Mention files skipped due to transfer overrides (levels 1-2).\n\
    STATS       Mention statistics at end of run (levels 1-3).\n\
    SYMSAFE     Mention symlinks that are unsafe.\n\
\n\
Flags may be written with 'no' or '-' prefixes (for example, --info=noprogress).\n\
Level suffixes may be used (for example, --info=stats2 or --info=flist0).\n";

pub(crate) const DEBUG_HELP_TEXT: &str = "The following --debug flags are supported:\n\
    all         Enable all diagnostic categories currently implemented.\n\
    none        Disable diagnostic output.\n\
    ACL         Debug extra ACL info.\n\
    BACKUP      Debug backup actions (levels 1-2).\n\
    BIND        Debug socket bind actions.\n\
    CHDIR       Debug when the current directory changes.\n\
    CONNECT     Debug connection events (levels 1-2).\n\
    CMD         Debug commands+options that are issued (levels 1-2).\n\
    DEL         Debug delete actions (levels 1-3).\n\
    DELTASUM    Debug delta-transfer checksumming (levels 1-4).\n\
    DUP         Debug weeding of duplicate names.\n\
    EXIT        Debug exit events (levels 1-3).\n\
    FILTER      Debug filter actions (levels 1-3).\n\
    FLIST       Debug file-list operations (levels 1-4).\n\
    FUZZY       Debug fuzzy scoring (levels 1-2).\n\
    GENR        Debug generator functions.\n\
    HASH        Debug hashtable code.\n\
    HLINK       Debug hard-link actions (levels 1-3).\n\
    ICONV       Debug iconv character conversions (levels 1-2).\n\
    IO          Debug I/O routines (levels 1-4).\n\
    NSTR        Debug negotiation strings.\n\
    OWN         Debug ownership changes in users & groups (levels 1-2).\n\
    PROTO       Debug protocol information.\n\
    RECV        Debug receiver functions.\n\
    SEND        Debug sender functions.\n\
    TIME        Debug setting of modified times (levels 1-2).\n\
\n\
Flags may be prefixed with 'no' or '-' to disable a category. Multiple flags\n\
may be combined by separating them with commas. Level suffixes may be used\n\
(for example, --debug=io2 or --debug=flist0).\n";

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    #[test]
    fn info_flag_parse_flag_and_level_default() {
        let settings = InfoFlagSettings::default();
        let (name, level) = settings.parse_flag_and_level("progress");
        assert_eq!(name, "progress");
        assert_eq!(level, 1);
    }

    #[test]
    fn info_flag_parse_flag_and_level_with_number() {
        let settings = InfoFlagSettings::default();
        let (name, level) = settings.parse_flag_and_level("progress2");
        assert_eq!(name, "progress");
        assert_eq!(level, 2);
    }

    #[test]
    fn info_flag_parse_flag_and_level_no_prefix() {
        let settings = InfoFlagSettings::default();
        let (name, level) = settings.parse_flag_and_level("noprogress");
        assert_eq!(name, "progress");
        assert_eq!(level, 0);
    }

    #[test]
    fn info_flag_parse_flag_and_level_dash_prefix() {
        let settings = InfoFlagSettings::default();
        let (name, level) = settings.parse_flag_and_level("-progress");
        assert_eq!(name, "progress");
        assert_eq!(level, 0);
    }

    #[test]
    fn info_flag_apply_help() {
        let mut settings = InfoFlagSettings::default();
        assert!(!settings.help_requested);
        settings.apply("help", "help").unwrap();
        assert!(settings.help_requested);
    }

    #[test]
    fn info_flag_apply_all() {
        let mut settings = InfoFlagSettings::default();
        settings.apply("all", "all").unwrap();
        assert_eq!(settings.progress, ProgressSetting::PerFile);
        assert_eq!(settings.stats, Some(1));
    }

    #[test]
    fn info_flag_apply_none() {
        let mut settings = InfoFlagSettings::default();
        settings.apply("all", "all").unwrap();
        settings.apply("none", "none").unwrap();
        assert_eq!(settings.progress, ProgressSetting::Disabled);
        assert_eq!(settings.stats, Some(0));
    }

    #[test]
    fn info_flag_apply_progress() {
        let mut settings = InfoFlagSettings::default();
        settings.apply("progress", "progress").unwrap();
        assert_eq!(settings.progress, ProgressSetting::PerFile);
    }

    #[test]
    fn info_flag_apply_progress2() {
        let mut settings = InfoFlagSettings::default();
        settings.apply("progress2", "progress2").unwrap();
        assert_eq!(settings.progress, ProgressSetting::Overall);
    }

    #[test]
    fn info_flag_apply_invalid() {
        let mut settings = InfoFlagSettings::default();
        let result = settings.apply("invalid", "invalid");
        assert!(result.is_err());
    }

    #[test]
    fn parse_info_flags_empty_value() {
        let values = vec![OsString::from("")];
        let result = parse_info_flags(&values);
        assert!(result.is_err());
    }

    #[test]
    fn parse_info_flags_comma_separated() {
        let values = vec![OsString::from("progress,stats")];
        let result = parse_info_flags(&values).unwrap();
        assert_eq!(result.progress, ProgressSetting::PerFile);
        assert_eq!(result.stats, Some(1));
    }

    #[test]
    fn debug_flag_parse_flag_and_level_default() {
        let settings = DebugFlagSettings::default();
        let (name, level) = settings.parse_flag_and_level("io");
        assert_eq!(name, "io");
        assert_eq!(level, 1);
    }

    #[test]
    fn debug_flag_parse_flag_and_level_with_number() {
        let settings = DebugFlagSettings::default();
        let (name, level) = settings.parse_flag_and_level("io3");
        assert_eq!(name, "io");
        assert_eq!(level, 3);
    }

    #[test]
    fn debug_flag_apply_all() {
        let mut settings = DebugFlagSettings::default();
        settings.apply("all", "all").unwrap();
        assert_eq!(settings.io, Some(1));
        assert_eq!(settings.flist, Some(1));
    }

    #[test]
    fn debug_flag_apply_none() {
        let mut settings = DebugFlagSettings::default();
        settings.apply("all", "all").unwrap();
        settings.apply("none", "none").unwrap();
        assert_eq!(settings.io, Some(0));
        assert_eq!(settings.flist, Some(0));
    }

    #[test]
    fn debug_flag_apply_io() {
        let mut settings = DebugFlagSettings::default();
        settings.apply("io", "io").unwrap();
        assert_eq!(settings.io, Some(1));
    }

    #[test]
    fn debug_flag_apply_io_level_too_high() {
        let mut settings = DebugFlagSettings::default();
        let result = settings.apply("io5", "io5");
        assert!(result.is_err());
    }

    #[test]
    fn debug_flag_apply_invalid() {
        let mut settings = DebugFlagSettings::default();
        let result = settings.apply("invalid", "invalid");
        assert!(result.is_err());
    }

    #[test]
    fn parse_debug_flags_empty_value() {
        let values = vec![OsString::from("")];
        let result = parse_debug_flags(&values);
        assert!(result.is_err());
    }

    #[test]
    fn parse_debug_flags_help_requested() {
        let values = vec![OsString::from("help")];
        let result = parse_debug_flags(&values).unwrap();
        assert!(result.help_requested);
    }

    #[test]
    fn parse_debug_flags_comma_separated() {
        let values = vec![OsString::from("io,flist")];
        let result = parse_debug_flags(&values).unwrap();
        assert_eq!(result.io, Some(1));
        assert_eq!(result.flist, Some(1));
    }

    #[test]
    fn info_flags_include_progress_basic() {
        let flags = vec![OsString::from("progress")];
        assert!(info_flags_include_progress(&flags));
    }

    #[test]
    fn info_flags_include_progress_no_prefix() {
        let flags = vec![OsString::from("noprogress")];
        assert!(info_flags_include_progress(&flags));
    }

    #[test]
    fn info_flags_include_progress_not_found() {
        let flags = vec![OsString::from("stats")];
        assert!(!info_flags_include_progress(&flags));
    }

    // ========================================================================
    // Comprehensive info flag parsing tests
    // ========================================================================

    #[test]
    fn info_flag_progress0_disables() {
        let mut settings = InfoFlagSettings::default();
        settings.apply("progress2", "progress2").unwrap();
        assert_eq!(settings.progress, ProgressSetting::Overall);
        settings.apply("progress0", "progress0").unwrap();
        assert_eq!(settings.progress, ProgressSetting::Disabled);
    }

    #[test]
    fn info_flag_progress3_rejected() {
        let mut settings = InfoFlagSettings::default();
        let result = settings.apply("progress3", "progress3");
        assert!(result.is_err());
    }

    #[test]
    fn info_flag_stats2() {
        let mut settings = InfoFlagSettings::default();
        settings.apply("stats2", "stats2").unwrap();
        assert_eq!(settings.stats, Some(2));
    }

    #[test]
    fn info_flag_stats3() {
        let mut settings = InfoFlagSettings::default();
        settings.apply("stats3", "stats3").unwrap();
        assert_eq!(settings.stats, Some(3));
    }

    #[test]
    fn info_flag_stats4_rejected() {
        let mut settings = InfoFlagSettings::default();
        let result = settings.apply("stats4", "stats4");
        assert!(result.is_err());
    }

    #[test]
    fn info_flag_name0() {
        let mut settings = InfoFlagSettings::default();
        settings.apply("name0", "name0").unwrap();
        assert_eq!(settings.name, Some(NameOutputLevel::Disabled));
    }

    #[test]
    fn info_flag_name1() {
        let mut settings = InfoFlagSettings::default();
        settings.apply("name", "name").unwrap();
        assert_eq!(settings.name, Some(NameOutputLevel::UpdatedOnly));
    }

    #[test]
    fn info_flag_name2() {
        let mut settings = InfoFlagSettings::default();
        settings.apply("name2", "name2").unwrap();
        assert_eq!(settings.name, Some(NameOutputLevel::UpdatedAndUnchanged));
    }

    #[test]
    fn info_flag_name_high_level_accepted() {
        let mut settings = InfoFlagSettings::default();
        // name levels >= 2 all map to UpdatedAndUnchanged
        settings.apply("name5", "name5").unwrap();
        assert_eq!(settings.name, Some(NameOutputLevel::UpdatedAndUnchanged));
    }

    #[test]
    fn info_flag_flist0() {
        let mut settings = InfoFlagSettings::default();
        settings.apply("flist0", "flist0").unwrap();
        assert_eq!(settings.flist, Some(0));
    }

    #[test]
    fn info_flag_flist2() {
        let mut settings = InfoFlagSettings::default();
        settings.apply("flist2", "flist2").unwrap();
        assert_eq!(settings.flist, Some(2));
    }

    #[test]
    fn info_flag_flist3_rejected() {
        let mut settings = InfoFlagSettings::default();
        let result = settings.apply("flist3", "flist3");
        assert!(result.is_err());
    }

    #[test]
    fn info_flag_misc2() {
        let mut settings = InfoFlagSettings::default();
        settings.apply("misc2", "misc2").unwrap();
        assert_eq!(settings.misc, Some(2));
    }

    #[test]
    fn info_flag_misc3_rejected() {
        let mut settings = InfoFlagSettings::default();
        let result = settings.apply("misc3", "misc3");
        assert!(result.is_err());
    }

    #[test]
    fn info_flag_skip2() {
        let mut settings = InfoFlagSettings::default();
        settings.apply("skip2", "skip2").unwrap();
        assert_eq!(settings.skip, Some(2));
    }

    #[test]
    fn info_flag_skip3_rejected() {
        let mut settings = InfoFlagSettings::default();
        let result = settings.apply("skip3", "skip3");
        assert!(result.is_err());
    }

    #[test]
    fn info_flag_backup_any_level() {
        let mut settings = InfoFlagSettings::default();
        settings.apply("backup", "backup").unwrap();
        assert_eq!(settings.backup, Some(1));
        settings.apply("backup5", "backup5").unwrap();
        assert_eq!(settings.backup, Some(5));
    }

    #[test]
    fn info_flag_copy_any_level() {
        let mut settings = InfoFlagSettings::default();
        settings.apply("copy", "copy").unwrap();
        assert_eq!(settings.copy, Some(1));
        settings.apply("copy3", "copy3").unwrap();
        assert_eq!(settings.copy, Some(3));
    }

    #[test]
    fn info_flag_del_any_level() {
        let mut settings = InfoFlagSettings::default();
        settings.apply("del", "del").unwrap();
        assert_eq!(settings.del, Some(1));
    }

    #[test]
    fn info_flag_mount_any_level() {
        let mut settings = InfoFlagSettings::default();
        settings.apply("mount", "mount").unwrap();
        assert_eq!(settings.mount, Some(1));
    }

    #[test]
    fn info_flag_nonreg_any_level() {
        let mut settings = InfoFlagSettings::default();
        settings.apply("nonreg", "nonreg").unwrap();
        assert_eq!(settings.nonreg, Some(1));
    }

    #[test]
    fn info_flag_remove_any_level() {
        let mut settings = InfoFlagSettings::default();
        settings.apply("remove", "remove").unwrap();
        assert_eq!(settings.remove, Some(1));
    }

    #[test]
    fn info_flag_symsafe_any_level() {
        let mut settings = InfoFlagSettings::default();
        settings.apply("symsafe", "symsafe").unwrap();
        assert_eq!(settings.symsafe, Some(1));
    }

    // ========================================================================
    // Negation prefix tests for all info flags
    // ========================================================================

    #[test]
    fn info_flag_no_prefix_copy() {
        let mut settings = InfoFlagSettings::default();
        settings.apply("nocopy", "nocopy").unwrap();
        assert_eq!(settings.copy, Some(0));
    }

    #[test]
    fn info_flag_dash_prefix_copy() {
        let mut settings = InfoFlagSettings::default();
        settings.apply("-copy", "-copy").unwrap();
        assert_eq!(settings.copy, Some(0));
    }

    #[test]
    fn info_flag_no_prefix_del() {
        let mut settings = InfoFlagSettings::default();
        settings.apply("nodel", "nodel").unwrap();
        assert_eq!(settings.del, Some(0));
    }

    #[test]
    fn info_flag_no_prefix_stats() {
        let mut settings = InfoFlagSettings::default();
        settings.apply("nostats", "nostats").unwrap();
        assert_eq!(settings.stats, Some(0));
    }

    #[test]
    fn info_flag_no_prefix_name() {
        let mut settings = InfoFlagSettings::default();
        settings.apply("noname", "noname").unwrap();
        assert_eq!(settings.name, Some(NameOutputLevel::Disabled));
    }

    #[test]
    fn info_flag_no_prefix_skip() {
        let mut settings = InfoFlagSettings::default();
        settings.apply("noskip", "noskip").unwrap();
        assert_eq!(settings.skip, Some(0));
    }

    #[test]
    fn info_flag_no_prefix_flist() {
        let mut settings = InfoFlagSettings::default();
        settings.apply("noflist", "noflist").unwrap();
        assert_eq!(settings.flist, Some(0));
    }

    #[test]
    fn info_flag_no_prefix_misc() {
        let mut settings = InfoFlagSettings::default();
        settings.apply("nomisc", "nomisc").unwrap();
        assert_eq!(settings.misc, Some(0));
    }

    #[test]
    fn info_flag_no_prefix_backup() {
        let mut settings = InfoFlagSettings::default();
        settings.apply("nobackup", "nobackup").unwrap();
        assert_eq!(settings.backup, Some(0));
    }

    #[test]
    fn info_flag_no_prefix_mount() {
        let mut settings = InfoFlagSettings::default();
        settings.apply("nomount", "nomount").unwrap();
        assert_eq!(settings.mount, Some(0));
    }

    #[test]
    fn info_flag_no_prefix_nonreg() {
        let mut settings = InfoFlagSettings::default();
        settings.apply("nononreg", "nononreg").unwrap();
        assert_eq!(settings.nonreg, Some(0));
    }

    #[test]
    fn info_flag_no_prefix_remove() {
        let mut settings = InfoFlagSettings::default();
        settings.apply("noremove", "noremove").unwrap();
        assert_eq!(settings.remove, Some(0));
    }

    #[test]
    fn info_flag_no_prefix_symsafe() {
        let mut settings = InfoFlagSettings::default();
        settings.apply("nosymsafe", "nosymsafe").unwrap();
        assert_eq!(settings.symsafe, Some(0));
    }

    // ========================================================================
    // Numeric ALL/NONE shorthand tests
    // ========================================================================

    #[test]
    fn info_flag_numeric_1_enables_all() {
        let mut settings = InfoFlagSettings::default();
        settings.apply("1", "1").unwrap();
        assert_eq!(settings.progress, ProgressSetting::PerFile);
        assert_eq!(settings.stats, Some(1));
        assert_eq!(settings.name, Some(NameOutputLevel::UpdatedOnly));
        assert_eq!(settings.backup, Some(1));
        assert_eq!(settings.copy, Some(1));
        assert_eq!(settings.del, Some(1));
        assert_eq!(settings.flist, Some(1));
        assert_eq!(settings.misc, Some(1));
        assert_eq!(settings.mount, Some(1));
        assert_eq!(settings.nonreg, Some(1));
        assert_eq!(settings.remove, Some(1));
        assert_eq!(settings.skip, Some(1));
        assert_eq!(settings.symsafe, Some(1));
    }

    #[test]
    fn info_flag_numeric_0_disables_all() {
        let mut settings = InfoFlagSettings::default();
        settings.apply("all", "all").unwrap();
        settings.apply("0", "0").unwrap();
        assert_eq!(settings.progress, ProgressSetting::Disabled);
        assert_eq!(settings.stats, Some(0));
        assert_eq!(settings.name, Some(NameOutputLevel::Disabled));
        assert_eq!(settings.backup, Some(0));
        assert_eq!(settings.copy, Some(0));
        assert_eq!(settings.del, Some(0));
        assert_eq!(settings.flist, Some(0));
        assert_eq!(settings.misc, Some(0));
        assert_eq!(settings.mount, Some(0));
        assert_eq!(settings.nonreg, Some(0));
        assert_eq!(settings.remove, Some(0));
        assert_eq!(settings.skip, Some(0));
        assert_eq!(settings.symsafe, Some(0));
    }

    // ========================================================================
    // Case insensitivity tests for keywords
    // ========================================================================

    #[test]
    fn info_flag_all_case_insensitive() {
        let mut settings = InfoFlagSettings::default();
        settings.apply("ALL", "ALL").unwrap();
        assert_eq!(settings.stats, Some(1));

        let mut settings = InfoFlagSettings::default();
        settings.apply("All", "All").unwrap();
        assert_eq!(settings.stats, Some(1));
    }

    #[test]
    fn info_flag_none_case_insensitive() {
        let mut settings = InfoFlagSettings::default();
        settings.apply("all", "all").unwrap();
        settings.apply("NONE", "NONE").unwrap();
        assert_eq!(settings.stats, Some(0));

        let mut settings = InfoFlagSettings::default();
        settings.apply("all", "all").unwrap();
        settings.apply("None", "None").unwrap();
        assert_eq!(settings.stats, Some(0));
    }

    #[test]
    fn info_flag_help_case_insensitive() {
        let mut settings = InfoFlagSettings::default();
        settings.apply("HELP", "HELP").unwrap();
        assert!(settings.help_requested);
    }

    // ========================================================================
    // Multiple value tests (simulating --info=X --info=Y)
    // ========================================================================

    #[test]
    fn parse_info_flags_multiple_values() {
        let values = vec![OsString::from("name"), OsString::from("stats2")];
        let result = parse_info_flags(&values).unwrap();
        assert_eq!(result.name, Some(NameOutputLevel::UpdatedOnly));
        assert_eq!(result.stats, Some(2));
    }

    #[test]
    fn parse_info_flags_multiple_with_comma_separated() {
        let values = vec![
            OsString::from("name,copy"),
            OsString::from("stats2,del"),
        ];
        let result = parse_info_flags(&values).unwrap();
        assert_eq!(result.name, Some(NameOutputLevel::UpdatedOnly));
        assert_eq!(result.copy, Some(1));
        assert_eq!(result.stats, Some(2));
        assert_eq!(result.del, Some(1));
    }

    #[test]
    fn parse_info_flags_later_overrides_earlier() {
        let values = vec![OsString::from("stats"), OsString::from("stats2")];
        let result = parse_info_flags(&values).unwrap();
        assert_eq!(result.stats, Some(2));
    }

    #[test]
    fn parse_info_flags_all_then_override() {
        let values = vec![OsString::from("all"), OsString::from("progress0")];
        let result = parse_info_flags(&values).unwrap();
        // all sets progress to PerFile, then progress0 disables it
        assert_eq!(result.progress, ProgressSetting::Disabled);
        // other flags still at level 1 from all
        assert_eq!(result.stats, Some(1));
        assert_eq!(result.name, Some(NameOutputLevel::UpdatedOnly));
    }

    #[test]
    fn parse_info_flags_none_then_enable() {
        let values = vec![OsString::from("none"), OsString::from("stats,name")];
        let result = parse_info_flags(&values).unwrap();
        // none disables all, then specific flags re-enabled
        assert_eq!(result.stats, Some(1));
        assert_eq!(result.name, Some(NameOutputLevel::UpdatedOnly));
        assert_eq!(result.progress, ProgressSetting::Disabled);
    }

    #[test]
    fn parse_info_flags_help_terminates_early() {
        let values = vec![OsString::from("help")];
        let result = parse_info_flags(&values).unwrap();
        assert!(result.help_requested);
    }

    // ========================================================================
    // Error message format tests
    // ========================================================================

    #[test]
    fn info_flag_error_message_contains_flag_name() {
        let mut settings = InfoFlagSettings::default();
        let result = settings.apply("bogus", "bogus");
        assert!(result.is_err());
        let msg = format!("{:?}", result.unwrap_err());
        assert!(msg.contains("bogus"), "error should mention the flag name");
    }

    #[test]
    fn info_flag_error_suggests_help() {
        let mut settings = InfoFlagSettings::default();
        let result = settings.apply("bogus", "bogus");
        assert!(result.is_err());
        let msg = format!("{:?}", result.unwrap_err());
        assert!(
            msg.contains("--info=help"),
            "error should suggest --info=help"
        );
    }

    // ========================================================================
    // Default state tests
    // ========================================================================

    #[test]
    fn info_flag_settings_default_is_unset() {
        let settings = InfoFlagSettings::default();
        assert_eq!(settings.progress, ProgressSetting::default());
        assert_eq!(settings.stats, None);
        assert_eq!(settings.name, None);
        assert_eq!(settings.backup, None);
        assert_eq!(settings.copy, None);
        assert_eq!(settings.del, None);
        assert_eq!(settings.flist, None);
        assert_eq!(settings.misc, None);
        assert_eq!(settings.mount, None);
        assert_eq!(settings.nonreg, None);
        assert_eq!(settings.remove, None);
        assert_eq!(settings.skip, None);
        assert_eq!(settings.symsafe, None);
        assert!(!settings.help_requested);
    }

    // ========================================================================
    // All flags enumeration test (ensure none are missed)
    // ========================================================================

    #[test]
    fn info_flag_all_keywords_accepted() {
        let keywords = [
            "backup", "copy", "del", "flist", "misc", "mount", "name",
            "nonreg", "progress", "remove", "skip", "stats", "symsafe",
        ];
        for keyword in &keywords {
            let mut settings = InfoFlagSettings::default();
            let result = settings.apply(keyword, keyword);
            assert!(
                result.is_ok(),
                "keyword '{keyword}' should be accepted but got: {result:?}"
            );
        }
    }

    // ========================================================================
    // info_flags_include_progress comprehensive tests
    // ========================================================================

    #[test]
    fn info_flags_include_progress_with_level() {
        let flags = vec![OsString::from("progress2")];
        assert!(info_flags_include_progress(&flags));
    }

    #[test]
    fn info_flags_include_progress_dash_prefix() {
        let flags = vec![OsString::from("-progress")];
        assert!(info_flags_include_progress(&flags));
    }

    #[test]
    fn info_flags_include_progress_in_comma_list() {
        let flags = vec![OsString::from("name,progress,stats")];
        assert!(info_flags_include_progress(&flags));
    }

    #[test]
    fn info_flags_include_progress_empty() {
        let flags: Vec<OsString> = vec![];
        assert!(!info_flags_include_progress(&flags));
    }

    #[test]
    fn info_flags_include_progress_multiple_values() {
        let flags = vec![OsString::from("name"), OsString::from("progress")];
        assert!(info_flags_include_progress(&flags));
    }

    // ========================================================================
    // Debug flag comprehensive tests
    // ========================================================================

    #[test]
    fn debug_flag_all_keywords_accepted() {
        let keywords = [
            "acl", "backup", "bind", "chdir", "connect", "cmd", "del",
            "deltasum", "dup", "exit", "filter", "flist", "fuzzy", "genr",
            "hash", "hlink", "iconv", "io", "nstr", "own", "proto", "recv",
            "send", "time",
        ];
        for keyword in &keywords {
            let mut settings = DebugFlagSettings::default();
            let result = settings.apply(keyword, keyword);
            assert!(
                result.is_ok(),
                "debug keyword '{keyword}' should be accepted but got: {result:?}"
            );
        }
    }

    #[test]
    fn debug_flag_level_limits() {
        let mut settings = DebugFlagSettings::default();

        // backup: max 2
        assert!(settings.apply("backup2", "backup2").is_ok());
        assert!(settings.apply("backup3", "backup3").is_err());

        // connect: max 2
        let mut settings = DebugFlagSettings::default();
        assert!(settings.apply("connect2", "connect2").is_ok());
        assert!(settings.apply("connect3", "connect3").is_err());

        // cmd: max 2
        let mut settings = DebugFlagSettings::default();
        assert!(settings.apply("cmd2", "cmd2").is_ok());
        assert!(settings.apply("cmd3", "cmd3").is_err());

        // del: max 3
        let mut settings = DebugFlagSettings::default();
        assert!(settings.apply("del3", "del3").is_ok());
        assert!(settings.apply("del4", "del4").is_err());

        // deltasum: max 4
        let mut settings = DebugFlagSettings::default();
        assert!(settings.apply("deltasum4", "deltasum4").is_ok());
        assert!(settings.apply("deltasum5", "deltasum5").is_err());

        // exit: max 3
        let mut settings = DebugFlagSettings::default();
        assert!(settings.apply("exit3", "exit3").is_ok());
        assert!(settings.apply("exit4", "exit4").is_err());

        // filter: max 3
        let mut settings = DebugFlagSettings::default();
        assert!(settings.apply("filter3", "filter3").is_ok());
        assert!(settings.apply("filter4", "filter4").is_err());

        // flist: max 4
        let mut settings = DebugFlagSettings::default();
        assert!(settings.apply("flist4", "flist4").is_ok());
        assert!(settings.apply("flist5", "flist5").is_err());

        // fuzzy: max 2
        let mut settings = DebugFlagSettings::default();
        assert!(settings.apply("fuzzy2", "fuzzy2").is_ok());
        assert!(settings.apply("fuzzy3", "fuzzy3").is_err());

        // hlink: max 3
        let mut settings = DebugFlagSettings::default();
        assert!(settings.apply("hlink3", "hlink3").is_ok());
        assert!(settings.apply("hlink4", "hlink4").is_err());

        // iconv: max 2
        let mut settings = DebugFlagSettings::default();
        assert!(settings.apply("iconv2", "iconv2").is_ok());
        assert!(settings.apply("iconv3", "iconv3").is_err());

        // io: max 4
        let mut settings = DebugFlagSettings::default();
        assert!(settings.apply("io4", "io4").is_ok());
        assert!(settings.apply("io5", "io5").is_err());

        // own: max 2
        let mut settings = DebugFlagSettings::default();
        assert!(settings.apply("own2", "own2").is_ok());
        assert!(settings.apply("own3", "own3").is_err());

        // time: max 2
        let mut settings = DebugFlagSettings::default();
        assert!(settings.apply("time2", "time2").is_ok());
        assert!(settings.apply("time3", "time3").is_err());
    }

    #[test]
    fn debug_flag_no_prefix_all_flags() {
        let keywords = [
            "acl", "backup", "bind", "chdir", "connect", "cmd", "del",
            "deltasum", "dup", "exit", "filter", "flist", "fuzzy", "genr",
            "hash", "hlink", "iconv", "io", "nstr", "own", "proto", "recv",
            "send", "time",
        ];
        for keyword in &keywords {
            let mut settings = DebugFlagSettings::default();
            let negated = format!("no{keyword}");
            let result = settings.apply(&negated, &negated);
            assert!(
                result.is_ok(),
                "negated debug keyword 'no{keyword}' should be accepted but got: {result:?}"
            );
        }
    }

    #[test]
    fn debug_flag_numeric_1_enables_all() {
        let mut settings = DebugFlagSettings::default();
        settings.apply("1", "1").unwrap();
        assert_eq!(settings.io, Some(1));
        assert_eq!(settings.proto, Some(1));
        assert_eq!(settings.flist, Some(1));
        assert_eq!(settings.acl, Some(1));
    }

    #[test]
    fn debug_flag_numeric_0_disables_all() {
        let mut settings = DebugFlagSettings::default();
        settings.apply("all", "all").unwrap();
        settings.apply("0", "0").unwrap();
        assert_eq!(settings.io, Some(0));
        assert_eq!(settings.proto, Some(0));
        assert_eq!(settings.flist, Some(0));
        assert_eq!(settings.acl, Some(0));
    }

    #[test]
    fn debug_flag_iter_enabled_flags_returns_nonzero() {
        let mut settings = DebugFlagSettings::default();
        settings.apply("io2", "io2").unwrap();
        settings.apply("flist", "flist").unwrap();
        settings.apply("del0", "del0").unwrap();

        let enabled: Vec<_> = settings.iter_enabled_flags().collect();
        assert!(enabled.contains(&("io", 2)));
        assert!(enabled.contains(&("flist", 1)));
        // del0 should not appear since level is 0
        assert!(!enabled.iter().any(|(name, _)| *name == "del"));
    }

    #[test]
    fn debug_flag_settings_default_is_unset() {
        let settings = DebugFlagSettings::default();
        assert_eq!(settings.acl, None);
        assert_eq!(settings.io, None);
        assert_eq!(settings.flist, None);
        assert!(!settings.help_requested);
    }

    #[test]
    fn parse_debug_flags_multiple_values() {
        let values = vec![OsString::from("io"), OsString::from("flist2")];
        let result = parse_debug_flags(&values).unwrap();
        assert_eq!(result.io, Some(1));
        assert_eq!(result.flist, Some(2));
    }

    #[test]
    fn parse_debug_flags_all_then_override() {
        let values = vec![OsString::from("all"), OsString::from("io0")];
        let result = parse_debug_flags(&values).unwrap();
        assert_eq!(result.io, Some(0));
        // Other flags remain at 1
        assert_eq!(result.flist, Some(1));
    }

    // ========================================================================
    // Help text content tests
    // ========================================================================

    #[test]
    fn info_help_text_lists_all_keywords() {
        let keywords = [
            "BACKUP", "COPY", "DEL", "FLIST", "MISC", "MOUNT", "NAME",
            "NONREG", "PROGRESS", "REMOVE", "SKIP", "STATS", "SYMSAFE",
        ];
        for keyword in &keywords {
            assert!(
                INFO_HELP_TEXT.contains(keyword),
                "INFO_HELP_TEXT should mention {keyword}"
            );
        }
    }

    #[test]
    fn info_help_text_mentions_all_and_none() {
        assert!(INFO_HELP_TEXT.contains("all"));
        assert!(INFO_HELP_TEXT.contains("none"));
    }

    #[test]
    fn debug_help_text_lists_all_keywords() {
        let keywords = [
            "ACL", "BACKUP", "BIND", "CHDIR", "CONNECT", "CMD", "DEL",
            "DELTASUM", "DUP", "EXIT", "FILTER", "FLIST", "FUZZY", "GENR",
            "HASH", "HLINK", "ICONV", "IO", "NSTR", "OWN", "PROTO", "RECV",
            "SEND", "TIME",
        ];
        for keyword in &keywords {
            assert!(
                DEBUG_HELP_TEXT.contains(keyword),
                "DEBUG_HELP_TEXT should mention {keyword}"
            );
        }
    }

    #[test]
    fn debug_help_text_mentions_all_and_none() {
        assert!(DEBUG_HELP_TEXT.contains("all"));
        assert!(DEBUG_HELP_TEXT.contains("none"));
    }
}

//! Verbosity configuration combining info and debug levels.
//!
//! This module provides [`VerbosityConfig`], the top-level container that
//! pairs [`InfoLevels`] with [`DebugLevels`]. It supports construction from
//! a global verbose count (matching upstream rsync's `-v` flag accumulation)
//! and fine-grained per-flag overrides (matching `--info=FLAGS` and
//! `--debug=FLAGS`).
//!
//! The cumulative mapping in [`VerbosityConfig::from_verbose_level`] mirrors
//! upstream's `set_output_verbosity()` (upstream: options.c:532), which
//! iterates `j = 0..=level` over the `info_verbosity[]` and
//! `debug_verbosity[]` tables (upstream: options.c:244-259).

use super::levels::{DebugFlag, DebugLevels, InfoFlag, InfoLevels};

/// Debug categories each `-v` adds, indexed by verbose count.
///
/// upstream: options.c:244-251 `debug_verbosity[]` - rows 0 and 1 are NULL,
/// so `-v` enables no debug category.
const DEBUG_VERBOSITY: [&[(DebugFlag, u8)]; 6] = {
    use DebugFlag::*;
    [
        &[],
        &[],
        &[
            (Bind, 1),
            (Cmd, 1),
            (Connect, 1),
            (Del, 1),
            (Deltasum, 1),
            (Dup, 1),
            (Filter, 1),
            (Flist, 1),
            (Iconv, 1),
        ],
        &[
            (Acl, 1),
            (Backup, 1),
            (Connect, 2),
            (Deltasum, 2),
            (Del, 2),
            (Exit, 1),
            (Filter, 2),
            (Flist, 2),
            (Fuzzy, 1),
            (Genr, 1),
            (Own, 1),
            (Recv, 1),
            (Send, 1),
            (Time, 1),
        ],
        &[
            (Cmd, 2),
            (Deltasum, 3),
            (Del, 3),
            (Exit, 2),
            (Flist, 3),
            (Iconv, 2),
            (Own, 2),
            (Proto, 1),
            (Time, 2),
        ],
        &[
            (Chdir, 1),
            (Deltasum, 4),
            (Flist, 4),
            (Fuzzy, 2),
            (Hash, 1),
            (Hlink, 1),
        ],
    ]
};

/// Highest verbose count with its own table row; larger counts clamp to it.
///
/// upstream: options.c:253 `#define MAX_VERBOSITY` - derived from the length
/// of `debug_verbosity[]`.
pub const MAX_VERBOSITY: u8 = (DEBUG_VERBOSITY.len() - 1) as u8;

/// Info categories each `-v` adds, indexed by verbose count.
///
/// upstream: options.c:255-259 `info_verbosity[1+MAX_VERBOSITY]` - rows past
/// 2 are NULL.
const INFO_VERBOSITY: [&[(InfoFlag, u8)]; DEBUG_VERBOSITY.len()] = {
    use InfoFlag::*;
    [
        &[(Nonreg, 1)],
        &[
            (Copy, 1),
            (Del, 1),
            (Flist, 1),
            (Misc, 1),
            (Name, 1),
            (Stats, 1),
            (Symsafe, 1),
        ],
        &[
            (Backup, 1),
            (Misc, 2),
            (Mount, 1),
            (Name, 2),
            (Remove, 1),
            (Skip, 1),
        ],
        &[],
        &[],
        &[],
    ]
};

/// Combined verbosity configuration for info and debug flags.
///
/// Holds one [`InfoLevels`] and one [`DebugLevels`] struct. Construct via
/// [`from_verbose_level`](Self::from_verbose_level) for `-v` flag mapping,
/// or build a default and apply individual flags via
/// [`apply_info_flag`](Self::apply_info_flag) /
/// [`apply_debug_flag`](Self::apply_debug_flag) for `--info=`/`--debug=`
/// overrides.
/// upstream: options.c struct that backs info_levels[] + debug_levels[]
#[derive(Clone, Default, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct VerbosityConfig {
    /// Per-category info verbosity levels.
    pub info: InfoLevels,
    /// Per-category debug verbosity levels.
    pub debug: DebugLevels,
}

impl VerbosityConfig {
    /// Create a new configuration from a verbose count.
    ///
    /// Applies rows `0..=level` of the info and debug verbosity tables in
    /// order, each entry overwriting its flag's level, so every level includes
    /// all lower ones. Counts above [`MAX_VERBOSITY`] clamp to it.
    /// upstream: options.c:532 set_output_verbosity()
    #[must_use]
    pub fn from_verbose_level(level: u8) -> Self {
        let mut config = Self::default();
        let rows = usize::from(level.min(MAX_VERBOSITY)) + 1;
        for (info_row, debug_row) in INFO_VERBOSITY.iter().zip(DEBUG_VERBOSITY).take(rows) {
            for &(flag, flag_level) in *info_row {
                config.info.set(flag, flag_level);
            }
            for &(flag, flag_level) in debug_row {
                config.debug.set(flag, flag_level);
            }
        }
        config
    }

    /// Apply a single info flag token (e.g., `"copy2"`, `"del"`).
    ///
    /// Parses the token into a flag name and optional numeric level suffix
    /// (defaulting to 1), then sets that flag. This implements the per-flag
    /// override syntax used by `--info=FLAGS`.
    /// upstream: options.c:parse_output_words()
    pub fn apply_info_flag(&mut self, token: &str) -> Result<(), String> {
        let (name, level) = parse_flag_token(token)?;

        // upstream: options.c:parse_output_words() matches names with
        // strncasecmp, so `--info=NAME` is case-insensitive (users and the
        // testsuite pass upper-case tokens like `--debug=FUZZY`).
        let name = name.to_ascii_lowercase();
        let flag = match name.as_str() {
            "backup" => InfoFlag::Backup,
            "copy" => InfoFlag::Copy,
            "del" => InfoFlag::Del,
            "flist" => InfoFlag::Flist,
            "misc" => InfoFlag::Misc,
            "mount" => InfoFlag::Mount,
            "name" => InfoFlag::Name,
            "nonreg" => InfoFlag::Nonreg,
            "progress" => InfoFlag::Progress,
            "remove" => InfoFlag::Remove,
            "skip" => InfoFlag::Skip,
            "stats" => InfoFlag::Stats,
            "symsafe" => InfoFlag::Symsafe,
            _ => return Err(format!("unknown info flag: {name}")),
        };

        self.info.set(flag, level);
        Ok(())
    }

    /// Apply a single debug flag token (e.g., `"recv2"`, `"flist"`).
    ///
    /// Parses the token into a flag name and optional numeric level suffix
    /// (defaulting to 1), then sets that flag. This implements the per-flag
    /// override syntax used by `--debug=FLAGS`.
    /// upstream: options.c:parse_output_words()
    pub fn apply_debug_flag(&mut self, token: &str) -> Result<(), String> {
        let (name, level) = parse_flag_token(token)?;

        // upstream: options.c:parse_output_words() matches names with
        // strncasecmp, so `--debug=NAME` is case-insensitive (users and the
        // testsuite pass upper-case tokens like `--debug=FUZZY`).
        let name = name.to_ascii_lowercase();
        let flag = match name.as_str() {
            "acl" => DebugFlag::Acl,
            "backup" => DebugFlag::Backup,
            "bind" => DebugFlag::Bind,
            "chdir" => DebugFlag::Chdir,
            "connect" => DebugFlag::Connect,
            "cmd" => DebugFlag::Cmd,
            "del" => DebugFlag::Del,
            "deltasum" => DebugFlag::Deltasum,
            "dup" => DebugFlag::Dup,
            "exit" => DebugFlag::Exit,
            "filter" => DebugFlag::Filter,
            "flist" => DebugFlag::Flist,
            "fuzzy" => DebugFlag::Fuzzy,
            "genr" => DebugFlag::Genr,
            "hash" => DebugFlag::Hash,
            "hlink" => DebugFlag::Hlink,
            "iconv" => DebugFlag::Iconv,
            "io" => DebugFlag::Io,
            "nstr" => DebugFlag::Nstr,
            "own" => DebugFlag::Own,
            "proto" => DebugFlag::Proto,
            "recv" => DebugFlag::Recv,
            "send" => DebugFlag::Send,
            "time" => DebugFlag::Time,
            // oc-specific accelerated-I/O fallback visibility categories.
            "iouring" => DebugFlag::Iouring,
            "clone" => DebugFlag::Clone,
            "sockopt" => DebugFlag::Sockopt,
            "iocp" => DebugFlag::Iocp,
            _ => return Err(format!("unknown debug flag: {name}")),
        };

        self.debug.set(flag, level);
        Ok(())
    }
}

/// Parse a flag token like `"copy2"` into `("copy", 2)` or `"del"` into `("del", 1)`.
///
/// Tokens without a trailing digit default to level 1, matching upstream
/// rsync's `parse_output_words()` behaviour (upstream: options.c).
fn parse_flag_token(token: &str) -> Result<(&str, u8), String> {
    if token.is_empty() {
        return Err("empty flag token".to_owned());
    }

    let digit_start = token.find(|c: char| c.is_ascii_digit());

    match digit_start {
        Some(pos) => {
            let name = &token[..pos];
            let level_str = &token[pos..];
            let level = level_str
                .parse::<u8>()
                .map_err(|_| format!("invalid level in flag: {token}"))?;
            Ok((name, level))
        }
        None => Ok((token, 1)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_from_verbose_level_1() {
        let config = VerbosityConfig::from_verbose_level(1);

        assert_eq!(config.info.nonreg, 1);
        assert_eq!(config.info.copy, 1);
        assert_eq!(config.info.del, 1);
        assert_eq!(config.info.flist, 1);
        assert_eq!(config.info.misc, 1);
        assert_eq!(config.info.name, 1);
        assert_eq!(config.info.stats, 1);
        assert_eq!(config.info.symsafe, 1);

        assert_eq!(config.info.backup, 0);
        assert_eq!(config.info.mount, 0);
        assert_eq!(config.debug.bind, 0);
    }

    #[test]
    fn test_from_verbose_level_2() {
        let config = VerbosityConfig::from_verbose_level(2);

        assert_eq!(config.info.misc, 2);
        assert_eq!(config.info.name, 2);
        assert_eq!(config.info.backup, 1);
        assert_eq!(config.info.mount, 1);
        assert_eq!(config.info.remove, 1);
        assert_eq!(config.info.skip, 1);

        assert_eq!(config.debug.bind, 1);
        assert_eq!(config.debug.cmd, 1);
        assert_eq!(config.debug.connect, 1);
        assert_eq!(config.debug.del, 1);
        assert_eq!(config.debug.deltasum, 1);
        assert_eq!(config.debug.dup, 1);
        assert_eq!(config.debug.filter, 1);
        assert_eq!(config.debug.flist, 1);
        assert_eq!(config.debug.iconv, 1);
    }

    #[test]
    fn test_parse_flag_token() {
        assert_eq!(parse_flag_token("copy").unwrap(), ("copy", 1));
        assert_eq!(parse_flag_token("copy2").unwrap(), ("copy", 2));
        assert_eq!(parse_flag_token("recv3").unwrap(), ("recv", 3));
        assert_eq!(parse_flag_token("flist10").unwrap(), ("flist", 10));
        assert!(parse_flag_token("").is_err());
    }

    #[test]
    fn test_apply_info_flag() {
        let mut config = VerbosityConfig::default();

        config.apply_info_flag("copy").unwrap();
        assert_eq!(config.info.copy, 1);

        config.apply_info_flag("copy2").unwrap();
        assert_eq!(config.info.copy, 2);

        config.apply_info_flag("stats3").unwrap();
        assert_eq!(config.info.stats, 3);

        assert!(config.apply_info_flag("invalid").is_err());
    }

    #[test]
    fn test_apply_debug_flag() {
        let mut config = VerbosityConfig::default();

        config.apply_debug_flag("recv").unwrap();
        assert_eq!(config.debug.recv, 1);

        config.apply_debug_flag("recv2").unwrap();
        assert_eq!(config.debug.recv, 2);

        config.apply_debug_flag("flist3").unwrap();
        assert_eq!(config.debug.flist, 3);

        assert!(config.apply_debug_flag("invalid").is_err());
    }

    #[test]
    fn test_apply_oc_accelerated_io_debug_flags() {
        let mut config = VerbosityConfig::default();

        config.apply_debug_flag("iouring").unwrap();
        config.apply_debug_flag("clone2").unwrap();
        config.apply_debug_flag("SOCKOPT").unwrap();
        config.apply_debug_flag("iocp").unwrap();

        assert_eq!(config.debug.iouring, 1);
        assert_eq!(config.debug.clone, 2);
        assert_eq!(config.debug.sockopt, 1);
        assert_eq!(config.debug.iocp, 1);
    }

    #[test]
    fn test_from_verbose_level_0() {
        let config = VerbosityConfig::from_verbose_level(0);

        assert_eq!(config.info.nonreg, 1);
        assert_eq!(config.info.copy, 0);
        assert_eq!(config.info.del, 0);
        assert_eq!(config.info.flist, 0);
        assert_eq!(config.info.misc, 0);
        assert_eq!(config.info.name, 0);
        assert_eq!(config.info.stats, 0);
        assert_eq!(config.info.symsafe, 0);
        assert_eq!(config.debug.bind, 0);
    }

    #[test]
    fn test_from_verbose_level_3() {
        let config = VerbosityConfig::from_verbose_level(3);

        assert_eq!(config.debug.connect, 2);
        assert_eq!(config.debug.del, 2);
        assert_eq!(config.debug.deltasum, 2);
        assert_eq!(config.debug.filter, 2);
        assert_eq!(config.debug.flist, 2);
        assert_eq!(config.debug.acl, 1);
        assert_eq!(config.debug.backup, 1);
        assert_eq!(config.debug.fuzzy, 1);
        assert_eq!(config.debug.genr, 1);
        assert_eq!(config.debug.own, 1);
        assert_eq!(config.debug.recv, 1);
        assert_eq!(config.debug.send, 1);
        assert_eq!(config.debug.time, 1);
        assert_eq!(config.debug.exit, 1);
    }

    #[test]
    fn test_from_verbose_level_4() {
        let config = VerbosityConfig::from_verbose_level(4);

        assert_eq!(config.debug.cmd, 2);
        assert_eq!(config.debug.del, 3);
        assert_eq!(config.debug.deltasum, 3);
        assert_eq!(config.debug.flist, 3);
        assert_eq!(config.debug.iconv, 2);
        assert_eq!(config.debug.own, 2);
        assert_eq!(config.debug.time, 2);
        assert_eq!(config.debug.exit, 2);
        assert_eq!(config.debug.proto, 1);
    }

    #[test]
    fn test_from_verbose_level_5_and_higher() {
        let config = VerbosityConfig::from_verbose_level(5);

        assert_eq!(config.debug.deltasum, 4);
        assert_eq!(config.debug.flist, 4);
        assert_eq!(config.debug.chdir, 1);
        assert_eq!(config.debug.hash, 1);
        assert_eq!(config.debug.hlink, 1);

        let config10 = VerbosityConfig::from_verbose_level(10);
        assert_eq!(config10.debug.deltasum, 4);
        assert_eq!(config10.debug.flist, 4);
        assert_eq!(config10.debug.chdir, 1);
        assert_eq!(config10.debug.hash, 1);
        assert_eq!(config10.debug.hlink, 1);
    }

    #[test]
    fn test_apply_all_info_flags() {
        let mut config = VerbosityConfig::default();

        config.apply_info_flag("backup").unwrap();
        assert_eq!(config.info.backup, 1);

        config.apply_info_flag("del2").unwrap();
        assert_eq!(config.info.del, 2);

        config.apply_info_flag("flist3").unwrap();
        assert_eq!(config.info.flist, 3);

        config.apply_info_flag("misc").unwrap();
        assert_eq!(config.info.misc, 1);

        config.apply_info_flag("mount2").unwrap();
        assert_eq!(config.info.mount, 2);

        config.apply_info_flag("name").unwrap();
        assert_eq!(config.info.name, 1);

        config.apply_info_flag("nonreg").unwrap();
        assert_eq!(config.info.nonreg, 1);

        config.apply_info_flag("progress2").unwrap();
        assert_eq!(config.info.progress, 2);

        config.apply_info_flag("remove").unwrap();
        assert_eq!(config.info.remove, 1);

        config.apply_info_flag("skip").unwrap();
        assert_eq!(config.info.skip, 1);

        config.apply_info_flag("symsafe").unwrap();
        assert_eq!(config.info.symsafe, 1);
    }

    #[test]
    fn test_apply_all_debug_flags() {
        let mut config = VerbosityConfig::default();

        config.apply_debug_flag("acl").unwrap();
        assert_eq!(config.debug.acl, 1);

        config.apply_debug_flag("backup2").unwrap();
        assert_eq!(config.debug.backup, 2);

        config.apply_debug_flag("bind").unwrap();
        assert_eq!(config.debug.bind, 1);

        config.apply_debug_flag("chdir3").unwrap();
        assert_eq!(config.debug.chdir, 3);

        config.apply_debug_flag("connect").unwrap();
        assert_eq!(config.debug.connect, 1);

        config.apply_debug_flag("cmd").unwrap();
        assert_eq!(config.debug.cmd, 1);

        config.apply_debug_flag("deltasum").unwrap();
        assert_eq!(config.debug.deltasum, 1);

        config.apply_debug_flag("dup").unwrap();
        assert_eq!(config.debug.dup, 1);

        config.apply_debug_flag("exit").unwrap();
        assert_eq!(config.debug.exit, 1);

        config.apply_debug_flag("filter").unwrap();
        assert_eq!(config.debug.filter, 1);

        config.apply_debug_flag("fuzzy").unwrap();
        assert_eq!(config.debug.fuzzy, 1);

        config.apply_debug_flag("genr").unwrap();
        assert_eq!(config.debug.genr, 1);

        config.apply_debug_flag("hash").unwrap();
        assert_eq!(config.debug.hash, 1);

        config.apply_debug_flag("hlink").unwrap();
        assert_eq!(config.debug.hlink, 1);

        config.apply_debug_flag("iconv").unwrap();
        assert_eq!(config.debug.iconv, 1);

        config.apply_debug_flag("io").unwrap();
        assert_eq!(config.debug.io, 1);

        config.apply_debug_flag("nstr").unwrap();
        assert_eq!(config.debug.nstr, 1);

        config.apply_debug_flag("own").unwrap();
        assert_eq!(config.debug.own, 1);

        config.apply_debug_flag("proto").unwrap();
        assert_eq!(config.debug.proto, 1);

        config.apply_debug_flag("send").unwrap();
        assert_eq!(config.debug.send, 1);

        config.apply_debug_flag("time").unwrap();
        assert_eq!(config.debug.time, 1);
    }

    /// upstream: options.c:parse_output_words() matches flag names with
    /// strncasecmp, so `--debug=FUZZY` (as typed by users and the fuzzy
    /// testsuite) must enable the flag exactly like `--debug=fuzzy`. A
    /// case-sensitive matcher silently dropped upper-case tokens, leaving
    /// `debug.fuzzy = 0` and suppressing the "fuzzy basis selected" line.
    #[test]
    fn apply_debug_flag_is_case_insensitive() {
        let mut config = VerbosityConfig::default();

        config.apply_debug_flag("FUZZY").unwrap();
        assert_eq!(config.debug.fuzzy, 1);

        // Level suffixes still parse against an upper-case name.
        config.apply_debug_flag("Fuzzy2").unwrap();
        assert_eq!(config.debug.fuzzy, 2);

        config.apply_debug_flag("DELTASUM").unwrap();
        assert_eq!(config.debug.deltasum, 1);
    }

    /// Info flags share the same case-insensitive matching contract.
    #[test]
    fn apply_info_flag_is_case_insensitive() {
        let mut config = VerbosityConfig::default();

        config.apply_info_flag("COPY").unwrap();
        assert_eq!(config.info.copy, 1);

        config.apply_info_flag("Progress").unwrap();
        assert_eq!(config.info.progress, 1);
    }

    #[test]
    fn test_verbosity_config_default() {
        let config = VerbosityConfig::default();
        assert_eq!(config.info.copy, 0);
        assert_eq!(config.info.del, 0);
        assert_eq!(config.debug.bind, 0);
        assert_eq!(config.debug.recv, 0);
    }

    #[test]
    fn test_verbosity_config_clone() {
        let mut config = VerbosityConfig::default();
        config.info.copy = 3;
        config.debug.recv = 2;

        let cloned = config;
        assert_eq!(cloned.info.copy, 3);
        assert_eq!(cloned.debug.recv, 2);
    }

    #[test]
    fn test_verbosity_config_debug_format() {
        let config = VerbosityConfig::default();
        let debug_str = format!("{config:?}");
        assert!(debug_str.contains("VerbosityConfig"));
        assert!(debug_str.contains("info"));
        assert!(debug_str.contains("debug"));
    }

    #[cfg(feature = "serde")]
    mod serde_tests {
        use super::*;
        use crate::{DebugFlag, InfoFlag};

        #[test]
        fn test_verbosity_config_serde_roundtrip() {
            let config = VerbosityConfig::from_verbose_level(2);

            let json = serde_json::to_string(&config).unwrap();
            let decoded: VerbosityConfig = serde_json::from_str(&json).unwrap();

            assert_eq!(config.info.copy, decoded.info.copy);
            assert_eq!(config.info.del, decoded.info.del);
            assert_eq!(config.debug.bind, decoded.debug.bind);
            assert_eq!(config.debug.flist, decoded.debug.flist);
        }

        #[test]
        fn test_info_flag_serde_roundtrip() {
            let flag = InfoFlag::Copy;
            let json = serde_json::to_string(&flag).unwrap();
            let decoded: InfoFlag = serde_json::from_str(&json).unwrap();
            assert_eq!(flag, decoded);
        }

        #[test]
        fn test_debug_flag_serde_roundtrip() {
            let flag = DebugFlag::Deltasum;
            let json = serde_json::to_string(&flag).unwrap();
            let decoded: DebugFlag = serde_json::from_str(&json).unwrap();
            assert_eq!(flag, decoded);
        }
    }
}

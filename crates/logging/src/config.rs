//! crates/logging/src/config.rs
//! Verbosity configuration combining info and debug levels.

use super::levels::{DebugFlag, DebugLevels, InfoFlag, InfoLevels};

/// Combined verbosity configuration for info and debug flags.
#[derive(Clone, Default, Debug)]
pub struct VerbosityConfig {
    /// Info flag levels.
    pub info: InfoLevels,
    /// Debug flag levels.
    pub debug: DebugLevels,
}

impl VerbosityConfig {
    /// Create a new configuration from a verbose level (0-5).
    /// Applies upstream rsync verbosity mapping.
    pub fn from_verbose_level(level: u8) -> Self {
        let mut config = Self::default();

        match level {
            0 => {
                config.info.nonreg = 1;
            }
            1 => {
                config.info.nonreg = 1;
                config.info.copy = 1;
                config.info.del = 1;
                config.info.flist = 1;
                config.info.misc = 1;
                config.info.name = 1;
                config.info.stats = 1;
                config.info.symsafe = 1;
            }
            2 => {
                config.info.nonreg = 1;
                config.info.copy = 1;
                config.info.del = 1;
                config.info.flist = 1;
                config.info.misc = 2;
                config.info.name = 2;
                config.info.stats = 1;
                config.info.symsafe = 1;
                config.info.backup = 2;
                config.info.mount = 2;
                config.info.remove = 2;
                config.info.skip = 2;
                config.debug.bind = 1;
                config.debug.cmd = 1;
                config.debug.connect = 1;
                config.debug.del = 1;
                config.debug.deltasum = 1;
                config.debug.dup = 1;
                config.debug.filter = 1;
                config.debug.flist = 1;
                config.debug.iconv = 1;
            }
            3 => {
                config.info.nonreg = 1;
                config.info.copy = 1;
                config.info.del = 1;
                config.info.flist = 1;
                config.info.misc = 2;
                config.info.name = 2;
                config.info.stats = 1;
                config.info.symsafe = 1;
                config.info.backup = 2;
                config.info.mount = 2;
                config.info.remove = 2;
                config.info.skip = 2;
                config.debug.bind = 1;
                config.debug.cmd = 1;
                config.debug.connect = 2;
                config.debug.del = 2;
                config.debug.deltasum = 2;
                config.debug.dup = 1;
                config.debug.filter = 2;
                config.debug.flist = 2;
                config.debug.iconv = 1;
                config.debug.acl = 1;
                config.debug.backup = 1;
                config.debug.fuzzy = 1;
                config.debug.genr = 1;
                config.debug.own = 1;
                config.debug.recv = 1;
                config.debug.send = 1;
                config.debug.time = 1;
                config.debug.exit = 2;
            }
            4 => {
                config.info.nonreg = 1;
                config.info.copy = 1;
                config.info.del = 1;
                config.info.flist = 1;
                config.info.misc = 2;
                config.info.name = 2;
                config.info.stats = 1;
                config.info.symsafe = 1;
                config.info.backup = 2;
                config.info.mount = 2;
                config.info.remove = 2;
                config.info.skip = 2;
                config.debug.bind = 1;
                config.debug.cmd = 2;
                config.debug.connect = 2;
                config.debug.del = 3;
                config.debug.deltasum = 3;
                config.debug.dup = 1;
                config.debug.filter = 2;
                config.debug.flist = 3;
                config.debug.iconv = 2;
                config.debug.acl = 1;
                config.debug.backup = 1;
                config.debug.fuzzy = 1;
                config.debug.genr = 1;
                config.debug.own = 2;
                config.debug.recv = 1;
                config.debug.send = 1;
                config.debug.time = 2;
                config.debug.exit = 3;
                config.debug.proto = 2;
            }
            _ => {
                // Level 5+
                config.info.nonreg = 1;
                config.info.copy = 1;
                config.info.del = 1;
                config.info.flist = 1;
                config.info.misc = 2;
                config.info.name = 2;
                config.info.stats = 1;
                config.info.symsafe = 1;
                config.info.backup = 2;
                config.info.mount = 2;
                config.info.remove = 2;
                config.info.skip = 2;
                config.debug.bind = 1;
                config.debug.cmd = 2;
                config.debug.connect = 2;
                config.debug.del = 3;
                config.debug.deltasum = 4;
                config.debug.dup = 1;
                config.debug.filter = 2;
                config.debug.flist = 4;
                config.debug.iconv = 2;
                config.debug.acl = 1;
                config.debug.backup = 1;
                config.debug.fuzzy = 1;
                config.debug.genr = 1;
                config.debug.own = 2;
                config.debug.recv = 1;
                config.debug.send = 1;
                config.debug.time = 2;
                config.debug.exit = 3;
                config.debug.proto = 2;
                config.debug.chdir = 1;
                config.debug.hash = 1;
                config.debug.hlink = 1;
            }
        }

        config
    }

    /// Apply a single info flag token (e.g., "copy2", "del").
    pub fn apply_info_flag(&mut self, token: &str) -> Result<(), String> {
        let (name, level) = parse_flag_token(token)?;

        let flag = match name {
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

    /// Apply a single debug flag token (e.g., "recv2", "flist").
    pub fn apply_debug_flag(&mut self, token: &str) -> Result<(), String> {
        let (name, level) = parse_flag_token(token)?;

        let flag = match name {
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
            _ => return Err(format!("unknown debug flag: {name}")),
        };

        self.debug.set(flag, level);
        Ok(())
    }
}

/// Parse a flag token like "copy2" into ("copy", 2) or "del" into ("del", 1).
fn parse_flag_token(token: &str) -> Result<(&str, u8), String> {
    if token.is_empty() {
        return Err("empty flag token".to_string());
    }

    // Find where the digits start
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
        None => {
            // No digits, default to level 1
            Ok((token, 1))
        }
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
        assert_eq!(config.info.backup, 2);
        assert_eq!(config.info.mount, 2);
        assert_eq!(config.info.remove, 2);
        assert_eq!(config.info.skip, 2);

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
}

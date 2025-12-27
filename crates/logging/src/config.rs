//! crates/logging/src/config.rs
//! Verbosity configuration combining info and debug levels.

use super::levels::{DebugFlag, DebugLevels, InfoFlag, InfoLevels};

/// Combined verbosity configuration for info and debug flags.
#[derive(Clone, Default, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
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

    #[test]
    fn test_from_verbose_level_0() {
        let config = VerbosityConfig::from_verbose_level(0);

        // Level 0 only sets nonreg
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

        // Level 3 has enhanced debug flags
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
        assert_eq!(config.debug.exit, 2);
    }

    #[test]
    fn test_from_verbose_level_4() {
        let config = VerbosityConfig::from_verbose_level(4);

        // Level 4 has further enhanced flags
        assert_eq!(config.debug.cmd, 2);
        assert_eq!(config.debug.del, 3);
        assert_eq!(config.debug.deltasum, 3);
        assert_eq!(config.debug.flist, 3);
        assert_eq!(config.debug.iconv, 2);
        assert_eq!(config.debug.own, 2);
        assert_eq!(config.debug.time, 2);
        assert_eq!(config.debug.exit, 3);
        assert_eq!(config.debug.proto, 2);
    }

    #[test]
    fn test_from_verbose_level_5_and_higher() {
        let config = VerbosityConfig::from_verbose_level(5);

        // Level 5+ has maximum debug output
        assert_eq!(config.debug.deltasum, 4);
        assert_eq!(config.debug.flist, 4);
        assert_eq!(config.debug.chdir, 1);
        assert_eq!(config.debug.hash, 1);
        assert_eq!(config.debug.hlink, 1);

        // Level 10 should also have max output
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

        // Test all info flags
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

        // Test all debug flags
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

        let cloned = config.clone();
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

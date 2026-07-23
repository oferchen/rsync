use std::ffi::OsString;

use core::{
    message::{Message, Role},
    rsync_error,
};

/// Parsed `--debug` flag settings controlling diagnostic output levels.
#[derive(Debug, Default)]
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
    // oc-specific accelerated-I/O fallback visibility categories.
    pub(crate) iouring: Option<u8>,
    pub(crate) clone: Option<u8>,
    pub(crate) sockopt: Option<u8>,
    pub(crate) iocp: Option<u8>,
    pub(crate) help_requested: bool,
}

/// Maximum debug output level. Levels above this are clamped rather than rejected.
/// upstream: options.c:245 - #define MAX_OUT_LEVEL 4
const MAX_OUT_LEVEL: u8 = 4;

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
            ("iouring", self.iouring),
            ("clone", self.clone),
            ("sockopt", self.sockopt),
            ("iocp", self.iocp),
        ]
        .into_iter()
        .filter_map(|(name, level)| level.filter(|&l| l > 0).map(|l| (name, l)))
    }

    /// Sets all debug flags to the given level.
    /// upstream: options.c:452-453 - "all" with numeric suffix sets every flag.
    fn set_all(&mut self, level: u8) {
        self.acl = Some(level);
        self.backup = Some(level);
        self.bind = Some(level);
        self.chdir = Some(level);
        self.connect = Some(level);
        self.cmd = Some(level);
        self.del = Some(level);
        self.deltasum = Some(level);
        self.dup = Some(level);
        self.exit = Some(level);
        self.filter = Some(level);
        self.flist = Some(level);
        self.fuzzy = Some(level);
        self.genr = Some(level);
        self.hash = Some(level);
        self.hlink = Some(level);
        self.iconv = Some(level);
        self.io = Some(level);
        self.nstr = Some(level);
        self.own = Some(level);
        self.proto = Some(level);
        self.recv = Some(level);
        self.send = Some(level);
        self.time = Some(level);
        self.iouring = Some(level);
        self.clone = Some(level);
        self.sockopt = Some(level);
        self.iocp = Some(level);
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
        self.iouring = Some(0);
        self.clone = Some(0);
        self.sockopt = Some(0);
        self.iocp = Some(0);
    }

    pub(super) fn apply(&mut self, token: &str, display: &str) -> Result<(), Message> {
        let lower = token.to_ascii_lowercase();

        // upstream: options.c:450-453 - "none" sets all to 0;
        // "all" with optional numeric suffix sets all flags to min(suffix, MAX_OUT_LEVEL).
        if lower == "0" || lower == "none" {
            self.disable_all();
            return Ok(());
        }

        if lower == "1"
            || lower == "all"
            || (lower.starts_with("all") && lower[3..].bytes().all(|b| b.is_ascii_digit()))
        {
            let level = if lower == "1" || lower == "all" {
                1
            } else {
                lower[3..].parse::<u8>().unwrap_or(1).min(MAX_OUT_LEVEL)
            };
            self.set_all(level);
            return Ok(());
        }

        let (normalized, level) = self.parse_flag_and_level(&lower);

        // upstream: options.c:444-445 - clamp to MAX_OUT_LEVEL rather than reject
        let level = level.min(MAX_OUT_LEVEL);

        match normalized {
            "acl" => self.acl = Some(level),
            "backup" => self.backup = Some(level),
            "bind" => self.bind = Some(level),
            "chdir" => self.chdir = Some(level),
            "connect" => self.connect = Some(level),
            "cmd" => self.cmd = Some(level),
            "del" => self.del = Some(level),
            "deltasum" => self.deltasum = Some(level),
            "dup" => self.dup = Some(level),
            "exit" => self.exit = Some(level),
            "filter" => self.filter = Some(level),
            "flist" => self.flist = Some(level),
            "fuzzy" => self.fuzzy = Some(level),
            "genr" => self.genr = Some(level),
            "hash" => self.hash = Some(level),
            "hlink" => self.hlink = Some(level),
            "iconv" => self.iconv = Some(level),
            "io" => self.io = Some(level),
            "nstr" => self.nstr = Some(level),
            "own" => self.own = Some(level),
            "proto" => self.proto = Some(level),
            "recv" => self.recv = Some(level),
            "send" => self.send = Some(level),
            "time" => self.time = Some(level),
            "iouring" => self.iouring = Some(level),
            "clone" => self.clone = Some(level),
            "sockopt" => self.sockopt = Some(level),
            "iocp" => self.iocp = Some(level),
            _ => return Err(debug_flag_error(display)),
        }

        Ok(())
    }

    /// Known debug flag names for disambiguating `no-` prefix vs flag names
    /// that might start with "no".
    const KNOWN_FLAGS: &'static [&'static str] = &[
        "acl", "backup", "bind", "chdir", "connect", "cmd", "del", "deltasum", "dup", "exit",
        "filter", "flist", "fuzzy", "genr", "hash", "hlink", "iconv", "io", "nstr", "own", "proto",
        "recv", "send", "time", "iouring", "clone", "sockopt", "iocp",
    ];

    pub(super) fn parse_flag_and_level<'a>(&self, input: &'a str) -> (&'a str, u8) {
        let base = input.trim_end_matches(|c: char| c.is_ascii_digit());
        if base != input {
            if Self::KNOWN_FLAGS.contains(&base) {
                let suffix = &input[base.len()..];
                let level = suffix.parse::<u8>().unwrap_or(1);
                return (base, level);
            }
        } else if Self::KNOWN_FLAGS.contains(&input) {
            return (input, 1);
        }

        let stripped = input.strip_prefix("no").or_else(|| input.strip_prefix('-'));

        if let Some(stripped) = stripped {
            return (stripped, 0);
        }

        (input, 1)
    }
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

/// Parses `--debug` flag values into resolved settings.
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

// upstream: options.c output_item_help (rsync-3.4.1:474-510). Reproduced
// byte-for-byte from upstream's runtime output so `--debug=help` matches
// `rsync --debug=help`. Layout matches `"%-10s %s\n"` from options.c:478.
// ALL/NONE descriptions inline the sentinel's `--debug` help
// (options.c:489-495). The per-verbosity summary lines are rendered by
// upstream's `make_output_option` over `debug_verbosity[]`
// (options.c:228-235) and emit names in `debug_words[]` order
// (options.c:289-315). Levels 0-1 are empty in `debug_verbosity[]`, so the
// summary block lists levels 2-5 only.
pub(crate) const DEBUG_HELP_TEXT: &str = "\
Use OPT or OPT1 for level 1 output, OPT2 for level 2, etc.; OPT0 silences.\n\
\n\
ACL        Debug extra ACL info\n\
BACKUP     Debug backup actions (levels 1-2)\n\
BIND       Debug socket bind actions\n\
CHDIR      Debug when the current directory changes\n\
CONNECT    Debug connection events (levels 1-2)\n\
CMD        Debug commands+options that are issued (levels 1-2)\n\
DEL        Debug delete actions (levels 1-3)\n\
DELTASUM   Debug delta-transfer checksumming (levels 1-4)\n\
DUP        Debug weeding of duplicate names\n\
EXIT       Debug exit events (levels 1-3)\n\
FILTER     Debug filter actions (levels 1-3)\n\
FLIST      Debug file-list operations (levels 1-4)\n\
FUZZY      Debug fuzzy scoring (levels 1-2)\n\
GENR       Debug generator functions\n\
HASH       Debug hashtable code\n\
HLINK      Debug hard-link actions (levels 1-3)\n\
ICONV      Debug iconv character conversions (levels 1-2)\n\
IO         Debug I/O routines (levels 1-4)\n\
NSTR       Debug negotiation strings\n\
OWN        Debug ownership changes in users & groups (levels 1-2)\n\
PROTO      Debug protocol information\n\
RECV       Debug receiver functions\n\
SEND       Debug sender functions\n\
TIME       Debug setting of modified times (levels 1-2)\n\
\n\
ALL        Set all --debug options (e.g. all4)\n\
NONE       Silence all --debug options (same as all0)\n\
HELP       Output this help message\n\
\n\
Options added at each level of verbosity:\n\
2) BIND,CONNECT,CMD,DEL,DELTASUM,DUP,FILTER,FLIST,ICONV\n\
3) ACL,BACKUP,CONNECT2,DEL2,DELTASUM2,EXIT,FILTER2,FLIST2,FUZZY,GENR,OWN,RECV,SEND,TIME\n\
4) CMD2,DEL3,DELTASUM3,EXIT2,FLIST3,ICONV2,OWN2,PROTO,TIME2\n\
5) CHDIR,DELTASUM4,FLIST4,FUZZY2,HASH,HLINK\n\
\n\
oc-rsync extensions (accelerated-I/O fallback visibility):\n\
IOURING    Debug io_uring probe and dispatch-vs-fallback decisions\n\
CLONE      Debug clonefile/reflink/copy_file_range CoW dispatch and fallback\n\
SOCKOPT    Debug TCP/socket tuning apply-or-skip decisions\n\
IOCP       Debug Windows IOCP dispatch and fallback\n";

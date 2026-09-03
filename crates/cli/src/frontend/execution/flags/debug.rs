use std::ffi::OsString;

use core::{
    message::{Message, Role},
    rsync_error,
};

use super::output_words::{self, OutputWord, TokenFlow};

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

    /// Applies one `--debug=` token.
    ///
    /// Returns [`TokenFlow::Stop`] for a `help` token: upstream prints the
    /// word table and calls `exit_cleanup(0)` right there (options.c:465-468),
    /// so every later token in the list is never examined.
    pub(super) fn apply(&mut self, token: &str) -> Result<TokenFlow, Message> {
        let (name, level) = match output_words::classify(token) {
            OutputWord::Help => {
                self.help_requested = true;
                return Ok(TokenFlow::Stop);
            }
            OutputWord::Every(level) => {
                self.set_all(level);
                return Ok(TokenFlow::Continue);
            }
            OutputWord::Named { name, level } => (name, level),
        };

        match name.to_ascii_lowercase().as_str() {
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
            _ => return Err(debug_flag_error(name)),
        }

        Ok(TokenFlow::Continue)
    }
}

/// Builds the unknown-item diagnostic for `--debug=`.
///
/// upstream: options.c:484-488 -
/// `rprintf(FERROR, "Unknown %s item: \"%.*s\"\n", words[j].help, len, str);`
/// followed by `exit_cleanup(RERR_SYNTAX)`. `words[j]` is the table's NULL
/// sentinel, whose `help` field is the literal `"--debug"` (options.c:333), and
/// `len` is the token length with the level suffix already stripped. The text
/// goes to `FERROR` (stderr) and the exit code is `RERR_SYNTAX` = 1
/// (errcode.h:25).
fn debug_flag_error(name: &str) -> Message {
    rsync_error!(1, format!("Unknown --debug item: \"{name}\"")).with_role(Role::Client)
}

/// Parses `--debug` flag values into resolved settings.
pub(crate) fn parse_debug_flags(values: &[OsString]) -> Result<DebugFlagSettings, Message> {
    let mut settings = DebugFlagSettings::default();

    for value in values {
        let text = value.to_string_lossy();
        // A `help` token stops the walk here as well as inside the value:
        // upstream exits from `parse_output_words` itself, so a later
        // `--debug=` argument is never parsed.
        let flow = output_words::for_each_token(&text, |token| settings.apply(token))?;
        if matches!(flow, TokenFlow::Stop) {
            break;
        }
    }

    Ok(settings)
}

/// Body of `--debug=help`, reproduced byte-for-byte from upstream.
///
/// upstream: options.c output_item_help (rsync-3.4.1:474-510). Reproduced
/// byte-for-byte from upstream's runtime output so `--debug=help` matches
/// `rsync --debug=help`. Layout matches `"%-10s %s\n"` from options.c:478.
/// ALL/NONE descriptions inline the sentinel's `--debug` help
/// (options.c:489-495). The per-verbosity summary lines are rendered by
/// upstream's `make_output_option` over `debug_verbosity[]`
/// (options.c:228-235) and emit names in `debug_words[]` order
/// (options.c:289-315). Levels 0-1 are empty in `debug_verbosity[]`, so the
/// summary block lists levels 2-5 only.
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

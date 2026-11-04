//! Value checks for the keywords the embedded reader does not act on.
//!
//! Upstream parses every keyword's value whether or not the client uses it,
//! so a value `ssh` refuses must refuse the file here too, and a value `ssh`
//! accepts must load. Each check below is one readconf.c arm of OpenSSH
//! 10.0p1, or the helper that arm calls, with upstream's own wording. The
//! value itself is dropped.
//!
//! The arms report in three ways, as upstream does: a counted error
//! (`goto out`, the file is refused once its last line is read), a fatal
//! error (`fatal()`, the load ends at once), and a bare note some helpers
//! print before the arm's own line (`vdollar_percent_expand`'s `error_f`).

use super::{Refusal, parse_connect_timeout, refuse};
use crate::ssh::config_options::{
    ForwardKind, Multistate, ValueKind, a2port, parse_flag_value, parse_int_value,
    parse_time_value, strtonum,
};
use crate::ssh::embedded::algorithms::key_types_valid;
use crate::ssh::embedded::error::SshError;

/// One line of a counted refusal.
enum Line {
    /// A reason upstream prints as `<file> line <n>: <reason>`.
    At(String),
    /// A helper's note, printed as is.
    Bare(String),
}

/// Why a value failed its arm.
enum Bad {
    /// Counted: the lines upstream prints, in order.
    Error(Vec<Line>),
    /// Fatal: the reason upstream prints after `<file> line <n>: `.
    Fatal(String),
}

impl Bad {
    fn at(reason: impl Into<String>) -> Self {
        Self::Error(vec![Line::At(reason.into())])
    }
}

/// Checks a line's `tokens` (and, for the arms that read it, the raw
/// `rest` of the line) the way the readconf.c arm for `kind` does, and
/// returns how many tokens the arm consumed - the caller refuses any left
/// over, as upstream's end-of-line check does (openssh/readconf.c:2529-2534).
pub(super) fn check(
    kind: ValueKind,
    keyword: &str,
    tokens: &[String],
    rest: &str,
    path: &str,
    linenum: usize,
) -> Result<usize, Refusal> {
    let first = tokens.first().map(String::as_str);
    let verdict = match kind {
        ValueKind::Flag => multistate(&["true", "false", "yes", "no"], first),
        // `multistate_addressfamily`, `multistate_pubkey_auth` and
        // `multistate_canonicalizehostname` (openssh/readconf.c:1038-1093).
        ValueKind::AddressFamily => multistate(&["inet", "inet6", "any"], first),
        ValueKind::PubkeyAuth => multistate(
            &["true", "false", "yes", "no", "unbound", "host-bound"],
            first,
        ),
        ValueKind::CanonicalizeHostname => {
            multistate(&["true", "false", "yes", "no", "always"], first)
        }
        // `RekeyLimit` reads a size and an optional interval; the reader's
        // own arm checks both (openssh/readconf.c:1362-1393).
        ValueKind::RekeyLimit => Ok(tokens.len().min(2)),
        ValueKind::Multistate(table) => multistate(table.keys(), first),
        ValueKind::Single => string(first),
        ValueKind::Time => parse_connect_timeout(first.unwrap_or_default())
            .map(|_| 1)
            .map_err(Bad::at),
        ValueKind::Int => parse_int_value(first.unwrap_or_default())
            .map(|_| 1)
            .map_err(Bad::at),
        ValueKind::IpQos => ip_qos(tokens),
        ValueKind::KeyTypes { ca_only } => key_types(first, ca_only),
        ValueKind::ForwardAgent => forward_agent(first),
        ValueKind::AddKeysToAgent => add_keys_to_agent(tokens),
        ValueKind::Forward(forward) => forwarding(forward, tokens),
        ValueKind::PermitRemoteOpen => permit_remote_open(keyword, tokens),
        ValueKind::EscapeChar => escape_char(first),
        ValueKind::LogFacility => named(LOG_FACILITIES, first, "unsupported log facility"),
        ValueKind::LogLevel => named(LOG_LEVELS, first, "unsupported log level"),
        ValueKind::LogVerbose => log_verbose(keyword, tokens),
        ValueKind::SendEnv => send_env(keyword, tokens),
        ValueKind::SetEnv => set_env(keyword, tokens),
        ValueKind::ControlPersist => control_persist(first),
        ValueKind::TunnelDevice => tunnel_device(first),
        ValueKind::StreamLocalBindMask => bind_mask(first),
        ValueKind::FingerprintHash => fingerprint_hash(first),
        ValueKind::ObscureKeystrokeTiming => keystroke_timing(tokens),
        ValueKind::ChannelTimeout => channel_timeout(keyword, tokens),
        ValueKind::VersionAddendum => version_addendum(keyword, rest, tokens),
        // The `parse_command` arms keep the whole line (argv_consume), and
        // the remaining shapes belong to arms the reader runs itself.
        ValueKind::Command
        | ValueKind::HostPatterns
        | ValueKind::MatchCriteria
        | ValueKind::IncludePatterns
        | ValueKind::KnownHostsFiles
        | ValueKind::CanonicalLists
        | ValueKind::Unknown => Ok(tokens.len()),
    };
    verdict.map_err(|bad| bad.into_refusal(path, linenum))
}

/// The `oPort` arm's value check (openssh/readconf.c:1563-1578): `a2port`
/// must resolve to a port above 0. Returns that port.
pub(super) fn port(arg: &str, path: &str, linenum: usize) -> Result<u16, Refusal> {
    a2port(arg)
        .filter(|&p| p > 0)
        .ok_or_else(|| Bad::at(format!("Bad port '{arg}'.")).into_refusal(path, linenum))
}

/// `parse_agent_path` for `IdentityAgent` (openssh/readconf.c:2381-2406).
pub(super) fn identity_agent(arg: &str, path: &str, linenum: usize) -> Result<(), Refusal> {
    agent_path(arg).map_err(|bad| bad.into_refusal(path, linenum))
}

impl Bad {
    /// The reader's form of this verdict for `path` line `linenum`.
    fn into_refusal(self, path: &str, linenum: usize) -> Refusal {
        match self {
            Bad::Error(lines) => Refusal::Counted(
                lines
                    .into_iter()
                    .map(|line| match line {
                        Line::At(reason) => refuse(path, linenum, reason).to_string(),
                        Line::Bare(note) => note,
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            Bad::Fatal(reason) => Refusal::Fatal(SshError::SshConfigFatal {
                reason: format!("{path} line {linenum}: {reason}"),
            }),
        }
    }
}

/// A non-empty token, or `None` for an absent or empty one - the
/// `!arg || *arg == '\0'` test most arms open with.
fn present(arg: Option<&str>) -> Option<&str> {
    arg.filter(|a| !a.is_empty())
}

/// `parse_multistate`: `parse_multistate_value` prints `missing argument.`
/// for an absent or empty token and the arm then adds its own line
/// (openssh/readconf.c:1103-1117, :1266-1280). An absent token prints as
/// glibc prints a NULL `%s`.
fn multistate(keys: &[&str], arg: Option<&str>) -> Result<usize, Bad> {
    let missing = present(arg).is_none();
    if present(arg).is_some_and(|a| keys.iter().any(|k| a.eq_ignore_ascii_case(k))) {
        return Ok(1);
    }
    let unsupported = Line::At(format!(
        "unsupported option \"{}\".",
        arg.unwrap_or("(null)")
    ));
    Err(Bad::Error(if missing {
        vec![Line::At("missing argument.".to_owned()), unsupported]
    } else {
        vec![unsupported]
    }))
}

/// `parse_string` (openssh/readconf.c:1442-1451).
fn string(arg: Option<&str>) -> Result<usize, Bad> {
    present(arg)
        .map(|_| 1)
        .ok_or_else(|| Bad::at("Missing argument."))
}

/// `parse_pubkey_algos` (openssh/readconf.c:1648-1664): a `-` list is not
/// checked, `+` and `^` are stripped first.
fn key_types(arg: Option<&str>, ca_only: bool) -> Result<usize, Bad> {
    let arg = present(arg).ok_or_else(|| Bad::at("Missing argument."))?;
    if arg.starts_with('-') || key_types_valid(arg.strip_prefix(['+', '^']).unwrap_or(arg), ca_only)
    {
        Ok(1)
    } else {
        Err(Bad::at(format!("Bad key types '{arg}'.")))
    }
}

/// `oForwardAgent` (openssh/readconf.c:1236-1264): a flag, else an agent
/// socket path checked by `parse_agent_path`.
fn forward_agent(arg: Option<&str>) -> Result<usize, Bad> {
    let arg = present(arg).ok_or_else(|| Bad::at("missing argument."))?;
    if parse_flag_value(arg).is_some() {
        return Ok(1);
    }
    agent_path(arg).map(|()| 1)
}

/// `parse_agent_path` (openssh/readconf.c:2389-2406): the `${VAR}`
/// expansion must succeed, and a legacy `$NAME` must be a valid name.
fn agent_path(arg: &str) -> Result<(), Bad> {
    let mut notes = Vec::new();
    if dollar_expand(arg.as_bytes(), &mut notes).is_none() {
        notes.push(Line::At(format!("Invalid environment expansion {arg}.")));
        return Err(Bad::Error(notes));
    }
    if arg.starts_with('$') && !arg[1..].starts_with('{') && !valid_env_name(&arg[1..]) {
        return Err(Bad::at(format!("Invalid environment name {arg}.")));
    }
    Ok(())
}

/// `valid_env_name` (openssh/misc.c:2449-2461).
fn valid_env_name(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// `dollar_expand` - `vdollar_percent_expand` with only the `${VAR}` arm
/// (openssh/misc.c:1284-1395, :1399-1411). Every note upstream's
/// `error_f` prints goes to `notes`; an unset variable does not stop the
/// scan, so each one is reported, and the result is `None`.
fn dollar_expand(s: &[u8], notes: &mut Vec<Line>) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len());
    let mut missing = false;
    let mut i = 0;
    while i < s.len() {
        if s[i] == b'$' && s.get(i + 1) == Some(&b'{') {
            let start = i + 2;
            let Some(len) = s[start..].iter().position(|&b| b == b'}') else {
                notes.push(Line::Bare(format!(
                    "vdollar_percent_expand: environment variable '{}' missing closing '}}'",
                    String::from_utf8_lossy(&s[start..])
                )));
                return None;
            };
            if len == 0 {
                notes.push(Line::Bare(
                    "vdollar_percent_expand: zero-length environment variable".to_owned(),
                ));
                return None;
            }
            let name = String::from_utf8_lossy(&s[start..start + len]);
            match std::env::var_os(name.as_ref()) {
                Some(value) => out.extend_from_slice(value.as_encoded_bytes()),
                None => {
                    notes.push(Line::Bare(format!(
                        "vdollar_percent_expand: env var ${{{name}}} has no value"
                    )));
                    missing = true;
                }
            }
            i = start + len + 1;
            continue;
        }
        out.push(s[i]);
        i += 1;
    }
    (!missing).then_some(out)
}

/// `oAddKeysToAgent` (openssh/readconf.c:2350-2379): a state, a lifetime,
/// or `confirm <lifetime>`.
fn add_keys_to_agent(tokens: &[String]) -> Result<usize, Bad> {
    let arg = tokens.first().map(String::as_str);
    let arg2 = tokens.get(1).map(String::as_str);
    let mut lines = Vec::new();
    let state = match present(arg) {
        None => {
            lines.push(Line::At("missing argument.".to_owned()));
            None
        }
        Some(a) => Multistate::YesNoAskConfirm
            .keys()
            .iter()
            .position(|k| a.eq_ignore_ascii_case(k)),
    };
    // `confirm` is the table's last row; `convtime(NULL)` is -1.
    let confirm = state == Some(Multistate::YesNoAskConfirm.keys().len() - 1);
    let lifetime_ok = |t: Option<&str>| t.and_then(parse_time_value).is_some();
    let reason = match (state, arg2) {
        (Some(_), Some(time)) if confirm => {
            (!lifetime_ok(Some(time))).then_some("invalid time value.")
        }
        (None, None) => (!lifetime_ok(arg)).then_some("unsupported option"),
        (None, Some(_)) | (Some(_), Some(_)) => Some("unsupported option"),
        (Some(_), None) => None,
    };
    match reason {
        None => Ok(tokens.len().min(2)),
        Some(reason) => {
            lines.push(Line::At(reason.to_owned()));
            Err(Bad::Error(lines))
        }
    }
}

/// The shared forwarding arm (openssh/readconf.c:1725-1770): a remote
/// forward with no target is dynamic, a local one is refused.
fn forwarding(kind: ForwardKind, tokens: &[String]) -> Result<usize, Bad> {
    /// `char fwdarg[256]` (openssh/readconf.c:1141), filled by `snprintf`
    /// or `strlcpy`.
    const FWDARG_MAX: usize = 255;
    let arg =
        present(tokens.first().map(String::as_str)).ok_or_else(|| Bad::at("Missing argument."))?;
    let remote = kind == ForwardKind::Remote;
    let mut dynamic = kind == ForwardKind::Dynamic;
    let mut consumed = 1;
    let mut spec = arg.as_bytes().to_vec();
    if !dynamic {
        match present(tokens.get(1).map(String::as_str)) {
            None if remote => dynamic = true,
            None => return Err(Bad::at("Missing target argument.")),
            Some(target) => {
                consumed = 2;
                spec = format!("{arg}:{target}").into_bytes();
            }
        }
    }
    spec.truncate(FWDARG_MAX);
    let mut notes = Vec::new();
    if parse_forward(&spec, dynamic, remote, &mut notes) {
        Ok(consumed)
    } else {
        notes.push(Line::At("Bad forwarding specification.".to_owned()));
        Err(Bad::Error(notes))
    }
}

/// `PORT_STREAMLOCAL` (openssh/channels.h).
const PORT_STREAMLOCAL: i32 = -2;
/// `NI_MAXHOST`.
const NI_MAXHOST: usize = 1025;
/// `PATH_MAX_SUN`, the platform's `sizeof(sun_path)` (openssh/readconf.h:22).
#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd",
    target_os = "dragonfly"
))]
const PATH_MAX_SUN: usize = 104;
#[cfg(not(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd",
    target_os = "dragonfly"
)))]
const PATH_MAX_SUN: usize = 108;

/// `a2port` as upstream's `int`: -1 when it does not resolve.
fn port_of(field: &[u8]) -> i32 {
    std::str::from_utf8(field)
        .ok()
        .and_then(a2port)
        .map_or(-1, i32::from)
}

/// One `parse_fwd_field` result (openssh/readconf.c:3163-3218).
struct FwdField {
    arg: Vec<u8>,
    ispath: bool,
}

/// `parse_fwd_field`: the next field from `cursor`, which becomes `None`
/// at the end of the string and is left at the field's start on a
/// malformed one.
fn parse_fwd_field(s: &[u8], cursor: &mut Option<usize>) -> Option<FwdField> {
    let start = (*cursor)?;
    if start >= s.len() {
        *cursor = None;
        return None;
    }
    if s[start] == b'[' {
        let close = s[start + 1..].iter().position(|&b| b == b']')? + start + 1;
        if !matches!(s.get(close + 1), None | Some(b':')) {
            return None;
        }
        let field = &s[start + 1..close];
        *cursor = Some((close + 2).min(s.len()));
        return Some(FwdField {
            arg: field.to_vec(),
            ispath: field.contains(&b'/'),
        });
    }
    let mut arg = Vec::new();
    let mut ispath = false;
    let mut i = start;
    while i < s.len() {
        match s[i] {
            b'\\' => {
                // The escaped byte is kept literally (memmove, then cp++).
                arg.push(*s.get(i + 1)?);
                i += 2;
                continue;
            }
            b'/' => ispath = true,
            b':' => {
                *cursor = Some(i + 1);
                return Some(FwdField { arg, ispath });
            }
            _ => {}
        }
        arg.push(s[i]);
        i += 1;
    }
    *cursor = Some(s.len());
    Some(FwdField { arg, ispath })
}

/// `parse_forward` (openssh/readconf.c:3229-3365), reduced to its verdict.
fn parse_forward(spec: &[u8], dynamic: bool, remote: bool, notes: &mut Vec<Line>) -> bool {
    let Some(expanded) = dollar_expand(spec, notes) else {
        return false;
    };
    let lead = expanded
        .iter()
        .take_while(|b| matches!(b, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r'))
        .count();
    let s = &expanded[lead..];
    let mut cursor = Some(0);
    let mut fields = Vec::new();
    while fields.len() < 4 {
        match parse_fwd_field(s, &mut cursor) {
            Some(field) => fields.push(field),
            None => break,
        }
    }
    if cursor.is_some_and(|c| c < s.len()) {
        return false;
    }

    // (listen host, listen port, listen path, connect host, connect port,
    // connect path), as `struct Forward` holds them.
    let socks = || Some(b"socks".to_vec());
    let (listen_host, listen_port, listen_path, connect_host, connect_port, connect_path) =
        match fields.as_slice() {
            [a] if a.ispath => (None, PORT_STREAMLOCAL, Some(&a.arg), socks(), 0, None),
            [a] => (None, port_of(&a.arg), None, socks(), 0, None),
            [a, b] if a.ispath && b.ispath => (
                None,
                PORT_STREAMLOCAL,
                Some(&a.arg),
                None,
                PORT_STREAMLOCAL,
                Some(&b.arg),
            ),
            [a, b] if b.ispath => (
                None,
                port_of(&a.arg),
                None,
                None,
                PORT_STREAMLOCAL,
                Some(&b.arg),
            ),
            [a, b] => (Some(&a.arg), port_of(&b.arg), None, socks(), 0, None),
            [a, b, c] if a.ispath => (
                None,
                PORT_STREAMLOCAL,
                Some(&a.arg),
                Some(b.arg.clone()),
                port_of(&c.arg),
                None,
            ),
            [a, b, c] if c.ispath => (
                Some(&a.arg),
                port_of(&b.arg),
                None,
                None,
                PORT_STREAMLOCAL,
                Some(&c.arg),
            ),
            [a, b, c] => (
                None,
                port_of(&a.arg),
                None,
                Some(b.arg.clone()),
                port_of(&c.arg),
                None,
            ),
            [a, b, c, d] => (
                Some(&a.arg),
                port_of(&b.arg),
                None,
                Some(c.arg.clone()),
                port_of(&d.arg),
                None,
            ),
            _ => return false,
        };
    let count = fields.len();
    if dynamic {
        if !(count == 1 || count == 2) {
            return false;
        }
    } else {
        if !(count == 3 || count == 4) && connect_path.is_none() && listen_path.is_none() {
            return false;
        }
        if connect_port <= 0 && connect_path.is_none() {
            return false;
        }
    }
    if (listen_port < 0 && listen_path.is_none()) || (!remote && listen_port == 0) {
        return false;
    }
    !(connect_host.is_some_and(|h| h.len() >= NI_MAXHOST)
        || connect_path.is_some_and(|p| p.len() >= PATH_MAX_SUN)
        || listen_host.is_some_and(|h| h.len() >= NI_MAXHOST)
        || listen_path.is_some_and(|p| p.len() >= PATH_MAX_SUN))
}

/// `oPermitRemoteOpen` (openssh/readconf.c:1772-1821). The fatal lines
/// name the option by `lookup_opcode_name`, its first table spelling.
fn permit_remote_open(keyword: &str, tokens: &[String]) -> Result<usize, Bad> {
    for (i, arg) in tokens.iter().enumerate() {
        if arg.eq_ignore_ascii_case("none") || arg.eq_ignore_ascii_case("any") {
            if i > 0 || tokens.len() > 1 {
                return Err(Bad::at(format!(
                    "keyword {keyword} \"{arg}\" argument must appear alone."
                )));
            }
            continue;
        }
        let Some(port) = hpdelim(arg.as_bytes()) else {
            return Err(Bad::Fatal("missing host in permitremoteopen".to_owned()));
        };
        if port.is_none_or(|p| p != b"*" && port_of(p) <= 0) {
            return Err(Bad::Fatal("bad port number in permitremoteopen".to_owned()));
        }
    }
    if tokens.is_empty() {
        return Err(Bad::Fatal(
            "missing permitremoteopen specification".to_owned(),
        ));
    }
    Ok(tokens.len())
}

/// `hpdelim` (openssh/misc.c:747-793): splits `host:rest` (`[host]:rest`
/// for a bracketed host) and returns the rest, `Some(None)` when there is
/// none, or `None` when the host part is malformed or ends in `/`.
fn hpdelim(s: &[u8]) -> Option<Option<&[u8]>> {
    let end = if s.first() == Some(&b'[') {
        s.iter().position(|&b| b == b']')? + 1
    } else {
        s.iter()
            .position(|&b| b == b':' || b == b'/')
            .unwrap_or(s.len())
    };
    match s.get(end) {
        None => Some(None),
        Some(b':') => Some(Some(&s[end + 1..])),
        Some(_) => None,
    }
}

/// `oEscapeChar` (openssh/readconf.c:1885-1912).
fn escape_char(arg: Option<&str>) -> Result<usize, Bad> {
    let arg = present(arg).ok_or_else(|| Bad::at("Missing argument."))?;
    let b = arg.as_bytes();
    if arg == "none" || b.len() == 1 || (b.len() == 2 && b[0] == b'^' && (64..128).contains(&b[1]))
    {
        Ok(1)
    } else {
        Err(Bad::at("Bad escape character."))
    }
}

/// `log_facilities` (openssh/log.c:75-93); `AUTHPRIV` exists wherever
/// `LOG_AUTHPRIV` does, as on every supported Unix.
const LOG_FACILITIES: &[&str] = &[
    "DAEMON", "USER", "AUTH", "AUTHPRIV", "LOCAL0", "LOCAL1", "LOCAL2", "LOCAL3", "LOCAL4",
    "LOCAL5", "LOCAL6", "LOCAL7",
];
/// `log_levels` (openssh/log.c:95-110).
const LOG_LEVELS: &[&str] = &[
    "QUIET", "FATAL", "ERROR", "INFO", "VERBOSE", "DEBUG", "DEBUG1", "DEBUG2", "DEBUG3",
];

/// `oLogFacility`/`oLogLevel` (openssh/readconf.c:1671-1695): a name from
/// `names`, compared with `strcasecmp`; an absent token prints `<NONE>`.
fn named(names: &[&str], arg: Option<&str>, refusal: &str) -> Result<usize, Bad> {
    if arg.is_some_and(|a| names.iter().any(|n| a.eq_ignore_ascii_case(n))) {
        Ok(1)
    } else {
        Err(Bad::at(format!("{refusal} '{}'", arg.unwrap_or("<NONE>"))))
    }
}

/// `oLogVerbose` (openssh/readconf.c:1697-1723).
fn log_verbose(keyword: &str, tokens: &[String]) -> Result<usize, Bad> {
    for (i, arg) in tokens.iter().enumerate() {
        if arg.is_empty() {
            return Err(Bad::at(format!("keyword {keyword} empty argument")));
        }
        if arg.eq_ignore_ascii_case("none") && (i > 0 || tokens.len() > 1) {
            return Err(Bad::at(format!(
                "keyword {keyword} \"none\" argument must appear alone."
            )));
        }
    }
    Ok(tokens.len())
}

/// `oSendEnv` (openssh/readconf.c:1930-1954).
fn send_env(keyword: &str, tokens: &[String]) -> Result<usize, Bad> {
    if tokens.iter().any(|arg| arg.is_empty() || arg.contains('=')) {
        return Err(Bad::at("Invalid environment name."));
    }
    if tokens.is_empty() {
        return Err(Bad::Fatal(format!("no {keyword} specified")));
    }
    Ok(tokens.len())
}

/// `oSetEnv` (openssh/readconf.c:1956-1983).
fn set_env(keyword: &str, tokens: &[String]) -> Result<usize, Bad> {
    if tokens.iter().any(|arg| !arg.contains('=')) {
        return Err(Bad::at("Invalid SetEnv."));
    }
    if tokens.is_empty() {
        return Err(Bad::Fatal(format!("no {keyword} specified")));
    }
    Ok(tokens.len())
}

/// `oControlPersist` (openssh/readconf.c:1994-2024): the words are
/// compared case-sensitively.
fn control_persist(arg: Option<&str>) -> Result<usize, Bad> {
    let arg = present(arg).ok_or_else(|| Bad::at("Missing ControlPersist argument."))?;
    if matches!(arg, "no" | "false" | "yes" | "true") || parse_time_value(arg).is_some() {
        Ok(1)
    } else {
        Err(Bad::at("Bad ControlPersist argument."))
    }
}

/// `oTunnelDevice` (openssh/readconf.c:2031-2048) over `a2tun`
/// (openssh/misc.c:551-580): `local[:remote]`, each `any` or a number up
/// to `SSH_TUNID_MAX`.
fn tunnel_device(arg: Option<&str>) -> Result<usize, Bad> {
    /// `SSH_TUNID_MAX` (openssh/misc.h:150).
    const SSH_TUNID_MAX: i64 = 0x7fff_ffff - 2;
    let arg = present(arg).ok_or_else(|| Bad::at("Missing argument."))?;
    let tun = |s: &str| s.eq_ignore_ascii_case("any") || strtonum(s, 0, SSH_TUNID_MAX).is_ok();
    let valid = match arg.split_once(':') {
        Some((local, remote)) => tun(local) && tun(remote),
        None => tun(arg),
    };
    if valid {
        Ok(1)
    } else {
        Err(Bad::at("Bad tun device."))
    }
}

/// `oStreamLocalBindMask` (openssh/readconf.c:2294-2308): `strtol(arg, 8)`
/// must read at least one octal digit and land in `0..=0777`; trailing
/// bytes are ignored.
fn bind_mask(arg: Option<&str>) -> Result<usize, Bad> {
    let arg = present(arg).ok_or_else(|| Bad::at("Missing StreamLocalBindMask argument."))?;
    let t = arg.trim_start_matches([' ', '\t', '\n', '\x0b', '\x0c', '\r']);
    let (negative, digits) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    let octal: Vec<u8> = digits
        .bytes()
        .take_while(|b| (b'0'..=b'7').contains(b))
        .collect();
    let value = octal.iter().fold(0_u64, |acc, d| {
        acc.saturating_mul(8).saturating_add(u64::from(d - b'0'))
    });
    if octal.is_empty() || (negative && value != 0) || value > 0o777 {
        Err(Bad::at("Bad mask."))
    } else {
        Ok(1)
    }
}

/// `oFingerprintHash` (openssh/readconf.c:2318-2333) over
/// `ssh_digest_alg_by_name` (openssh/digest-openssl.c:58-89).
fn fingerprint_hash(arg: Option<&str>) -> Result<usize, Bad> {
    let arg = present(arg).ok_or_else(|| Bad::at("Missing argument."))?;
    if ["MD5", "SHA1", "SHA256", "SHA384", "SHA512"]
        .iter()
        .any(|d| arg.eq_ignore_ascii_case(d))
    {
        Ok(1)
    } else {
        Err(Bad::at(format!("Invalid hash algorithm \"{arg}\".")))
    }
}

/// `oObscureKeystrokeTiming` (openssh/readconf.c:2416-2456).
fn keystroke_timing(tokens: &[String]) -> Result<usize, Bad> {
    let mut set = false;
    for arg in tokens {
        if set {
            return Err(Bad::at("invalid arguments"));
        }
        if !matches!(arg.as_str(), "yes" | "true" | "no" | "false") {
            let Some(ms) = arg.strip_prefix("interval:") else {
                return Err(Bad::at(format!("unsupported argument \"{arg}\"")));
            };
            // `atoi_err` (openssh/misc.c:2463-2472).
            let value = match ms {
                "" => Err("missing"),
                ms => strtonum(ms, 0, i64::from(i32::MAX)),
            }
            .map_err(|errstr| Bad::at(format!("integer value {errstr}.")))?;
            if !(1..=1000).contains(&value) {
                return Err(Bad::at("value out of range."));
            }
        }
        set = true;
    }
    if set {
        Ok(tokens.len())
    } else {
        Err(Bad::at("missing argument"))
    }
}

/// `oChannelTimeout` (openssh/readconf.c:2458-2487) over
/// `parse_pattern_interval` (openssh/misc.c:2548-2576).
fn channel_timeout(keyword: &str, tokens: &[String]) -> Result<usize, Bad> {
    for (i, arg) in tokens.iter().enumerate() {
        if arg.eq_ignore_ascii_case("none") {
            if i > 0 || tokens.len() > 1 {
                return Err(Bad::at(format!(
                    "keyword {keyword} \"none\" argument must appear alone."
                )));
            }
            continue;
        }
        let valid = arg.split_once('=').is_some_and(|(kind, interval)| {
            !kind.is_empty() && parse_time_value(interval).is_some()
        });
        if !valid {
            return Err(Bad::Fatal(format!("invalid channel timeout {arg}")));
        }
    }
    if tokens.is_empty() {
        return Err(Bad::Fatal(format!("no {keyword} specified")));
    }
    Ok(tokens.len())
}

/// `oVersionAddendum` (openssh/readconf.c:2489-2509): the raw rest of the
/// line, refused if it carries a carriage return.
fn version_addendum(keyword: &str, rest: &str, tokens: &[String]) -> Result<usize, Bad> {
    if rest.contains('\r') {
        return Err(Bad::Fatal(format!("Invalid {keyword} argument")));
    }
    Ok(tokens.len())
}

/// `oIPQoS` (openssh/readconf.c:2154-2173) over `parse_ipqos`
/// (openssh/misc.c:1864-1912): one or two classes or DSCP values.
fn ip_qos(tokens: &[String]) -> Result<usize, Bad> {
    const CLASSES: &[&str] = &[
        "none",
        "af11",
        "af12",
        "af13",
        "af21",
        "af22",
        "af23",
        "af31",
        "af32",
        "af33",
        "af41",
        "af42",
        "af43",
        "cs0",
        "cs1",
        "cs2",
        "cs3",
        "cs4",
        "cs5",
        "cs6",
        "cs7",
        "ef",
        "le",
        "lowdelay",
        "throughput",
        "reliability",
    ];
    let valid =
        |v: &str| CLASSES.iter().any(|c| v.eq_ignore_ascii_case(c)) || strtonum(v, 0, 255).is_ok();
    // The first value is required; the second is optional.
    let first = tokens.first().map(String::as_str);
    let second = tokens.get(1).map(String::as_str);
    for value in std::iter::once(first).chain(second.map(Some)) {
        if !value.is_some_and(valid) {
            return Err(Bad::at(format!(
                "Bad IPQoS value: {}",
                value.unwrap_or("(null)")
            )));
        }
    }
    Ok(tokens.len().min(2))
}

#[cfg(test)]
mod tests {
    use super::super::resolve_host_str;

    /// The verdict on a one-line config, in `ssh -G`'s words for a file
    /// named `c`: `None` when it loads.
    fn verdict(line: &str) -> Option<String> {
        resolve_host_str(&format!("{line}\n"), "a")
            .err()
            .map(|err| err.to_string().replace("<ssh_config>", "c"))
    }

    /// A counted refusal: the lines, then upstream's terminating line.
    fn counted(lines: &[&str]) -> Option<String> {
        let mut text: String = lines.iter().map(|l| format!("c line 1: {l}\n")).collect();
        text.push_str("c: terminating, 1 bad configuration options");
        Some(text)
    }

    /// Values every arm accepts, each measured to load on the 10.0p1
    /// `ssh -G`. GSSAPI is pinned to its `parse_flag` arm
    /// (openssh/readconf.c:1319-1325) by unit test alone: the keyword table
    /// maps it there only in a GSSAPI build (:212-226), which the pinned
    /// oracle is not, and oc models the GSSAPI build distributions ship.
    #[test]
    fn values_upstream_accepts_load() {
        for line in [
            "ForwardAgent yes",
            "ForwardAgent /run/agent.sock",
            "ForwardAgent $1bad",
            "ForwardAgent %d/x",
            "GSSAPIAuthentication yes",
            "GSSAPIDelegateCredentials NO",
            "PKCS11Provider /usr/lib/p11.so",
            "SmartcardDevice /dev/sc0",
            "LocalForward 8080 host:80",
            "LocalForward [::1]:8080 host:80",
            "LocalForward /tmp/s host:80",
            "LocalForward 8080 host:http",
            "LocalForward h:8080 host:80",
            "RemoteForward 8080",
            "RemoteForward 0 h:1",
            "DynamicForward 1080",
            "DynamicForward host:1080",
            "PermitRemoteOpen any",
            "PermitRemoteOpen h:* [::1]:22",
            "EscapeChar ~",
            "EscapeChar none",
            "EscapeChar ^A",
            "EscapeChar ^",
            "StreamLocalBindMask 0177",
            "StreamLocalBindMask 0778",
            "StreamLocalBindMask \" 7\"",
            "TunnelDevice any:any",
            "TunnelDevice 1:2",
            "TunnelDevice 2147483645",
            "FingerprintHash md5",
            "SyslogFacility AUTHPRIV",
            "LogLevel debug3",
            "LogVerbose a b",
            "LogVerbose none",
            "SendEnv LANG LC_* -LANG",
            "SetEnv A=B C=D A=E",
            "ControlPersist 10m",
            "ObscureKeystrokeTiming interval:1000",
            "ChannelTimeout session=5m none2=1",
            "ChannelTimeout none",
            "VersionAddendum hello world",
            "AddKeysToAgent confirm 5m",
            "AddKeysToAgent 5m",
            "AddKeysToAgent ask",
            "CASignatureAlgorithms ssh-ed25519,*cert*",
            "PubkeyAcceptedAlgorithms +ssh-rsa",
            "HostbasedAcceptedAlgorithms -nope",
            "StrictHostKeyChecking accept-new",
            "ControlMaster autoask",
            "Tunnel point-to-point",
            "RequestTTY force",
            "SessionType subsystem",
            "UpdateHostKeys ASK",
            "ForwardX11Timeout none",
            "TCPKeepAlive no",
            "Compression yes",
            "IPQoS af11 cs1",
            "IPQoS 255",
            "ConnectionAttempts 3",
            "BindInterface eth0",
            "LocalCommand a b c",
            "KnownHostsCommand x y",
        ] {
            assert_eq!(verdict(line), None, "{line}");
        }
    }

    /// Each arm's refusal text, measured on the 10.0p1 `ssh -G` (file
    /// `c`). GSSAPI is pinned to `parse_flag` (openssh/readconf.c:212-226,
    /// :1319-1325), as for the accepted values; its text matches a
    /// GSSAPI-enabled `ssh` (measured on the host's distribution build).
    #[test]
    fn values_upstream_refuses_fail_with_its_words() {
        let cases: &[(&str, Option<String>)] = &[
            (
                "GSSAPIAuthentication maybe",
                counted(&["unsupported option \"maybe\"."]),
            ),
            (
                "ForwardX11 #c",
                counted(&["missing argument.", "unsupported option \"(null)\"."]),
            ),
            (
                "ForwardX11 \"\"",
                counted(&["missing argument.", "unsupported option \"\"."]),
            ),
            (
                "ForwardX11 yes no",
                counted(&["keyword forwardx11 extra arguments at end of line"]),
            ),
            ("PKCS11Provider \"\"", counted(&["Missing argument."])),
            ("ForwardX11Timeout x", counted(&["invalid time value."])),
            (
                "TCPKeepAlive transport",
                counted(&["unsupported option \"transport\"."]),
            ),
            (
                "Compression true",
                counted(&["unsupported option \"true\"."]),
            ),
            ("ConnectionAttempts x", counted(&["integer value invalid."])),
            (
                "ConnectionAttempts 3 4",
                counted(&["keyword connectionattempts extra arguments at end of line"]),
            ),
            ("IPQoS bogus", counted(&["Bad IPQoS value: bogus"])),
            ("IPQoS af11 bogus", counted(&["Bad IPQoS value: bogus"])),
            ("IPQoS 256", counted(&["Bad IPQoS value: 256"])),
            (
                "IPQoS 1 2 3",
                counted(&["keyword ipqos extra arguments at end of line"]),
            ),
            (
                "BindInterface a b",
                counted(&["keyword bindinterface extra arguments at end of line"]),
            ),
            ("LocalForward 8080", counted(&["Missing target argument."])),
            (
                "LocalForward 0 host:80",
                counted(&["Bad forwarding specification."]),
            ),
            (
                "LocalForward 8080 host:0",
                counted(&["Bad forwarding specification."]),
            ),
            (
                "LocalForward 8080 host:nosuchsvc589",
                counted(&["Bad forwarding specification."]),
            ),
            (
                "LocalForward 8080 host:80:extra",
                counted(&["Bad forwarding specification."]),
            ),
            (
                "LocalForward h:8080 host:80:x",
                counted(&["Bad forwarding specification."]),
            ),
            (
                "DynamicForward 0",
                counted(&["Bad forwarding specification."]),
            ),
            (
                "DynamicForward a:b:c",
                counted(&["Bad forwarding specification."]),
            ),
            ("DynamicForward \"\"", counted(&["Missing argument."])),
            (
                "PermitRemoteOpen any h:1",
                counted(&["keyword permitremoteopen \"any\" argument must appear alone."]),
            ),
            (
                "PermitRemoteOpen h:1 none",
                counted(&["keyword permitremoteopen \"none\" argument must appear alone."]),
            ),
            (
                "PermitRemoteOpen h",
                Some("c line 1: bad port number in permitremoteopen".to_owned()),
            ),
            (
                "PermitRemoteOpen h:x",
                Some("c line 1: bad port number in permitremoteopen".to_owned()),
            ),
            (
                "PermitRemoteOpen h/22",
                Some("c line 1: missing host in permitremoteopen".to_owned()),
            ),
            ("EscapeChar ab", counted(&["Bad escape character."])),
            ("EscapeChar \"\"", counted(&["Missing argument."])),
            ("StreamLocalBindMask x", counted(&["Bad mask."])),
            ("StreamLocalBindMask -1", counted(&["Bad mask."])),
            ("StreamLocalBindMask 1000", counted(&["Bad mask."])),
            ("TunnelDevice x", counted(&["Bad tun device."])),
            ("TunnelDevice 1:x", counted(&["Bad tun device."])),
            ("TunnelDevice 2147483646", counted(&["Bad tun device."])),
            (
                "FingerprintHash sha3",
                counted(&["Invalid hash algorithm \"sha3\"."]),
            ),
            (
                "SyslogFacility nope",
                counted(&["unsupported log facility 'nope'"]),
            ),
            ("LogLevel loud", counted(&["unsupported log level 'loud'"])),
            (
                "LogVerbose none a",
                counted(&["keyword logverbose \"none\" argument must appear alone."]),
            ),
            (
                "LogVerbose \"\"",
                counted(&["keyword logverbose empty argument"]),
            ),
            ("SendEnv A=B", counted(&["Invalid environment name."])),
            ("SendEnv \"\"", counted(&["Invalid environment name."])),
            ("SetEnv A", counted(&["Invalid SetEnv."])),
            (
                "SetEnv #c",
                Some("c line 1: no setenv specified".to_owned()),
            ),
            (
                "SendEnv #c",
                Some("c line 1: no sendenv specified".to_owned()),
            ),
            (
                "ControlPersist YES",
                counted(&["Bad ControlPersist argument."]),
            ),
            (
                "ControlPersist 10x",
                counted(&["Bad ControlPersist argument."]),
            ),
            (
                "ControlPersist \"\"",
                counted(&["Missing ControlPersist argument."]),
            ),
            (
                "ObscureKeystrokeTiming interval:0",
                counted(&["value out of range."]),
            ),
            (
                "ObscureKeystrokeTiming interval:1001",
                counted(&["value out of range."]),
            ),
            (
                "ObscureKeystrokeTiming interval:x",
                counted(&["integer value invalid."]),
            ),
            (
                "ObscureKeystrokeTiming interval:",
                counted(&["integer value missing."]),
            ),
            (
                "ObscureKeystrokeTiming maybe",
                counted(&["unsupported argument \"maybe\""]),
            ),
            (
                "ObscureKeystrokeTiming yes no",
                counted(&["invalid arguments"]),
            ),
            ("ObscureKeystrokeTiming #c", counted(&["missing argument"])),
            (
                "ChannelTimeout none x=1",
                counted(&["keyword channeltimeout \"none\" argument must appear alone."]),
            ),
            (
                "ChannelTimeout session",
                Some("c line 1: invalid channel timeout session".to_owned()),
            ),
            (
                "ChannelTimeout a=x",
                Some("c line 1: invalid channel timeout a=x".to_owned()),
            ),
            (
                "ChannelTimeout #c",
                Some("c line 1: no channeltimeout specified".to_owned()),
            ),
            (
                "ChannelTimeout \"=5\"",
                Some("c line 1: invalid channel timeout =5".to_owned()),
            ),
            (
                "VersionAddendum a\rb",
                Some("c line 1: Invalid versionaddendum argument".to_owned()),
            ),
            (
                "AddKeysToAgent confirm x",
                counted(&["invalid time value."]),
            ),
            ("AddKeysToAgent x", counted(&["unsupported option"])),
            ("AddKeysToAgent yes 5m", counted(&["unsupported option"])),
            (
                "AddKeysToAgent #c",
                counted(&["missing argument.", "unsupported option"]),
            ),
            (
                "CASignatureAlgorithms ssh-ed25519-cert-v01@openssh.com",
                counted(&["Bad key types 'ssh-ed25519-cert-v01@openssh.com'."]),
            ),
            (
                "PubkeyAcceptedKeyTypes nope",
                counted(&["Bad key types 'nope'."]),
            ),
            (
                "StrictHostKeyChecking maybe",
                counted(&["unsupported option \"maybe\"."]),
            ),
            (
                "ControlMaster maybe",
                counted(&["unsupported option \"maybe\"."]),
            ),
            ("Tunnel maybe", counted(&["unsupported option \"maybe\"."])),
            (
                "RequestTTY maybe",
                counted(&["unsupported option \"maybe\"."]),
            ),
            (
                "SessionType maybe",
                counted(&["unsupported option \"maybe\"."]),
            ),
            (
                "VerifyHostKeyDNS maybe",
                counted(&["unsupported option \"maybe\"."]),
            ),
        ];
        for (line, expected) in cases {
            assert_eq!(&verdict(line), expected, "{line}");
        }
    }

    /// `parse_agent_path` and `parse_forward` run `dollar_expand`, whose
    /// notes precede the arm's own line (openssh/misc.c:1324-1342).
    #[test]
    fn a_failed_env_expansion_prints_the_expanders_note_first() {
        let cases: &[(&str, &str)] = &[
            (
                "ForwardAgent ${OC_T589_UNSET}",
                "vdollar_percent_expand: env var ${OC_T589_UNSET} has no value\n\
                 c line 1: Invalid environment expansion ${OC_T589_UNSET}.",
            ),
            (
                "ForwardAgent ${",
                "vdollar_percent_expand: environment variable '' missing closing '}'\n\
                 c line 1: Invalid environment expansion ${.",
            ),
            (
                "ForwardAgent ${}",
                "vdollar_percent_expand: zero-length environment variable\n\
                 c line 1: Invalid environment expansion ${}.",
            ),
            (
                "LocalForward ${OC_T589_UNSET} h:1",
                "vdollar_percent_expand: env var ${OC_T589_UNSET} has no value\n\
                 c line 1: Bad forwarding specification.",
            ),
            ("ForwardAgent $", "c line 1: Invalid environment name $."),
        ];
        for (line, text) in cases {
            let expected = format!("{text}\nc: terminating, 1 bad configuration options");
            assert_eq!(verdict(line).as_deref(), Some(expected.as_str()), "{line}");
        }
    }

    /// The keywords the reader acts on run the same checks first, so an
    /// absent, empty, malformed or surplus value is refused as `ssh -G`
    /// refuses it (openssh/readconf.c:1103-1117, :1442-1451, :1563-1578,
    /// :2389-2406, :2529-2534).
    #[test]
    fn acted_on_values_are_checked_like_upstream() {
        let cases: &[(&str, Option<String>)] = &[
            (
                "IdentitiesOnly #c",
                counted(&["missing argument.", "unsupported option \"(null)\"."]),
            ),
            (
                "IdentitiesOnly maybe",
                counted(&["unsupported option \"maybe\"."]),
            ),
            (
                "AddressFamily #c",
                counted(&["missing argument.", "unsupported option \"(null)\"."]),
            ),
            (
                "PubkeyAuthentication \"\"",
                counted(&["missing argument.", "unsupported option \"\"."]),
            ),
            (
                "ProxyUseFdpass #c",
                counted(&["missing argument.", "unsupported option \"(null)\"."]),
            ),
            (
                "CheckHostIP yes no",
                counted(&["keyword checkhostip extra arguments at end of line"]),
            ),
            (
                "CanonicalizeHostname x y",
                counted(&["unsupported option \"x\"."]),
            ),
            ("Ciphers \"\"", counted(&["Missing argument."])),
            (
                "RekeyLimit 1G 1h x",
                counted(&["keyword rekeylimit extra arguments at end of line"]),
            ),
            (
                "Port 22 23",
                counted(&["keyword port extra arguments at end of line"]),
            ),
            ("Port abc", counted(&["Bad port 'abc'."])),
            ("Port 0", counted(&["Bad port '0'."])),
            ("Port 0x10", counted(&["Bad port '0x10'."])),
            ("User \"\"", counted(&["Missing argument."])),
            (
                "User a b",
                counted(&["keyword user extra arguments at end of line"]),
            ),
            ("IdentityFile \"\"", counted(&["Missing argument."])),
            ("IdentityAgent $", counted(&["Invalid environment name $."])),
            ("Port 65535", None),
            ("RekeyLimit 1G 1h", None),
        ];
        for (line, expected) in cases {
            assert_eq!(&verdict(line), expected, "{line}");
        }
        let unset = verdict("IdentityAgent ${OC_T589_UNSET}");
        assert_eq!(
            unset.as_deref(),
            Some(
                "vdollar_percent_expand: env var ${OC_T589_UNSET} has no value\n\
                 c line 1: Invalid environment expansion ${OC_T589_UNSET}.\n\
                 c: terminating, 1 bad configuration options"
            )
        );
    }

    /// `a2port` resolves a service name through the services file, as
    /// `getservbyname(s, "tcp")` does (openssh/misc.c:536-548).
    #[test]
    fn a_port_service_name_resolves() {
        let resolved = resolve_host_str("Port ssh\n", "a").expect("loads");
        assert_eq!(resolved.port, Some(22));
    }

    /// The 101st identity or certificate file refuses its line
    /// (openssh/readconf.c:1402-1409, :1422-1430).
    #[test]
    fn the_hundred_and_first_key_file_is_refused() {
        for (keyword, what) in [
            ("IdentityFile", "identity"),
            ("CertificateFile", "certificate"),
        ] {
            let text: String = (1..=101).map(|i| format!("{keyword} /k{i}\n")).collect();
            let err = resolve_host_str(&text, "a")
                .expect_err("refused")
                .to_string();
            assert_eq!(
                err,
                format!(
                    "<ssh_config> line 101: Too many {what} files specified (max 100).\n\
                     <ssh_config>: terminating, 1 bad configuration options"
                ),
                "{keyword}"
            );
            let hundred: String = (1..=100).map(|i| format!("{keyword} /k{i}\n")).collect();
            assert!(resolve_host_str(&hundred, "a").is_ok(), "{keyword}");
        }
    }

    /// A refused unread value inside an inactive block still refuses the
    /// file: upstream checks before the `*activep` gate.
    #[test]
    fn an_inactive_block_still_checks_unread_values() {
        let err =
            resolve_host_str("Host other\n  ForwardAgent maybe/x${\n", "a").expect_err("refused");
        assert!(
            err.to_string().contains("Invalid environment expansion"),
            "{err}"
        );
    }
}

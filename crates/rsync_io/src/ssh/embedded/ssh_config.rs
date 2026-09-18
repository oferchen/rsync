//! Minimal `~/.ssh/config` parser for the embedded russh transport.
//!
//! Recognises the subset of OpenSSH client directives that the embedded
//! transport can act on: `Host`, `Hostname`, `User`, `Port`, `IdentityFile`,
//! `IdentitiesOnly`, `IdentityAgent`, `ConnectTimeout`, and the
//! connection-establishment trio the russh transport has a knob for -
//! `AddressFamily`, `ServerAliveInterval`, `ServerAliveCountMax`. Other
//! recognised connection keywords (`BindAddress`, `BindInterface`,
//! `ConnectionAttempts`, `IPQoS`, `TCPKeepAlive`) are accepted by the shared
//! table but not acted on here, because the embedded transport exposes no
//! matching knob. Unknown directives are skipped
//! silently. Wildcards in `Host` patterns follow OpenSSH semantics: `*`
//! matches any sequence, `?` matches any single character, `!pattern`
//! negates a match for that block.
//!
//! # The keyword set is not declared here
//!
//! Which spelling means which directive comes from
//! [`crate::ssh::config_options`], the one option table oc has, mirroring
//! upstream's single `keywords[]` scan (openssh/readconf.c:1194
//! `parse_token`). This module owns only what it DOES with a resolved
//! opcode - the `Host`-only scope and the refusal policy below - and it
//! shares the `Key Value` split, the yes/no parse and the glob matcher
//! with the compression reader (`crate::ssh::config_lookup`). Adding a
//! directive is one row in the table plus one arm here, and the other
//! reader cannot drift away from the spelling.
//!
//! # Tokenisation
//!
//! Every line's value half is split by [`argv_split`], the shared port of
//! upstream's `argv_split` - the same call readconf makes once per line at
//! openssh/readconf.c:1196. That is where quoting, backslash escapes and
//! `#`-comment termination are decided; this module only consumes the
//! resulting tokens. In particular a `#` is a comment **only at a token
//! boundary**, so `HostName x#y` resolves to `x#y`.
//!
//! # Failure mode
//!
//! A missing or unreadable file is still an empty result, but a file that
//! real `ssh` would REFUSE is now an error rather than silently-empty.
//! Upstream counts bad lines and then aborts the load outright
//! (openssh/readconf.c:2667), so the connection never happens; resolving
//! to different connection parameters than `ssh` would have used is the
//! worse outcome. Four refusals are mirrored, each with upstream's own
//! wording, and each fires whether or not the surrounding `Host` block
//! matches - upstream parses every line and gates only the ASSIGNMENT on
//! `*activep`:
//!
//! * `invalid quotes` - a quote left open (openssh/readconf.c:1196-1199).
//! * `keyword host empty argument` - an empty `Host` token
//!   (openssh/readconf.c:1832-1836).
//! * `Missing argument.` / `missing argument.` / `missing time value.` -
//!   a directive whose value tokenised to nothing, e.g. `Port #comment`.
//!   The wording is upstream's own and splits by value SHAPE, not by
//!   keyword: single-token options say `Missing` (openssh/readconf.c:1364),
//!   multistate flags say `missing` (openssh/readconf.c:1108), and time
//!   values have their own line (openssh/readconf.c:1219). Measured
//!   against real `ssh -G` for every keyword below. The wording hangs off
//!   the option table's `ValueKind` so a keyword added later cannot pick
//!   the wrong one.
//! * `invalid time value.` - a `ConnectTimeout` value `convtime` rejects
//!   (openssh/readconf.c:1224-1227).
//!
//! The caller (`SshConfig::apply_ssh_config`) merges the resolved
//! directives into the existing config, with the rule that any value
//! already set on the URL wins over the config file.

use std::fs;
use std::path::{Path, PathBuf};

use super::error::SshError;
use crate::ssh::argv_split::argv_split;
use crate::ssh::config_files::{ConfigFile, check_default_user_config_perms, home_dir as env_home};
use crate::ssh::config_options::{
    AddressFamily, Opcode, glob_matches, parse_address_family, parse_flag_value, parse_int_value,
    parse_time_value, parse_token, split_directive,
};

/// upstream's `READCONF_MAX_DEPTH` - the nested-`Include` ceiling
/// (openssh/readconf.c:2561). A top-level file is depth 0, each `Include`
/// recursion adds one, and a file entered at depth > 16 is a fatal config
/// error (openssh/readconf.c:2573-2574).
const READCONF_MAX_DEPTH: u32 = 16;

/// The directory a RELATIVE `Include` anchors against in a SYSTEM config -
/// upstream's `SSHDIR` (openssh/readconf.c:2103), the parent of
/// `/etc/ssh/ssh_config`.
const SYSTEM_SSH_DIR: &str = "/etc/ssh";

/// Placeholder file name used when a caller supplies config text with no
/// path of its own. Upstream always has a real filename to name in a
/// refusal (openssh/readconf.c:1197 `"%s line %d: ..."`), so the
/// text-only entry point substitutes this rather than printing nothing.
#[cfg(test)]
const INLINE_CONFIG_NAME: &str = "<ssh_config>";

/// Resolved directives for a single host alias, merged in declaration order
/// across every matching `Host` block. Only the directives the embedded
/// russh transport understands are tracked.
#[derive(Debug, Default, Clone)]
pub(super) struct ResolvedHost {
    pub hostname: Option<String>,
    pub user: Option<String>,
    pub port: Option<u16>,
    pub identity_files: Vec<PathBuf>,
    pub identities_only: Option<bool>,
    pub identity_agent: Option<String>,
    /// `ConnectTimeout` in whole seconds. `None` means no directive
    /// obtained a value - which is also what `ConnectTimeout none`
    /// resolves to, since upstream maps `none` onto the same -1 the unset
    /// slot holds (openssh/readconf.c:1222-1223, :2734).
    pub connect_timeout: Option<u32>,
    /// `AddressFamily` selection. `None` means no directive claimed the
    /// slot (openssh/readconf.c:1903-1906).
    pub address_family: Option<AddressFamily>,
    /// `ServerAliveInterval` in whole seconds. `None` means no directive
    /// obtained a value - the same collapse as `ConnectTimeout`, since the
    /// shared `parse_time` arm maps `none` onto the unset -1
    /// (openssh/readconf.c:1214-1228, :1916).
    pub server_alive_interval: Option<u32>,
    /// `ServerAliveCountMax`. `None` means no directive claimed the slot
    /// (openssh/readconf.c:1920, `parse_int`).
    pub server_alive_count_max: Option<u32>,
    /// `ProxyCommand`, the rest of the line taken verbatim. `None` means no
    /// active line claimed the proxy slot. The literal `none` is stored as
    /// written and later read as "no proxy" (openssh/ssh.c:1298
    /// `option_clear_or_none`), so the value carries through unchanged.
    pub proxy_command: Option<String>,
    /// `ProxyJump`, the raw `[user@]host[:port][,...]` chain. `None` means no
    /// active line claimed the proxy slot. Lowered to a `ProxyCommand`
    /// equivalent at dial time, mirroring openssh/ssh.c:1310-1360.
    pub jump_hosts: Option<String>,
    /// `ProxyUseFdpass`. `None` means no active line claimed the slot
    /// (openssh/readconf.c:1471, `parse_flag`).
    pub proxy_use_fdpass: Option<bool>,
}

impl ResolvedHost {
    /// Whether an active line has already claimed the shared proxy slot.
    ///
    /// `ProxyCommand` and `ProxyJump` are mutually exclusive and resolve
    /// first-obtained-wins across BOTH keywords, exactly as upstream: the
    /// `oProxyCommand` arm ignores its value once a jump is set
    /// (openssh/readconf.c:1466-1467), and `parse_jump` no-ops once either
    /// is set (openssh/readconf.c:1730 `active &= o->proxy_command == NULL
    /// && o->jump_host == NULL`).
    fn proxy_claimed(&self) -> bool {
        self.proxy_command.is_some() || self.jump_hosts.is_some()
    }
}

/// Parses `path` and returns the directives that apply to `host_alias`.
///
/// Returns `Ok(ResolvedHost::default())` when the file cannot be read -
/// upstream treats an absent config the same way
/// (openssh/readconf.c:2632-2633 `if ((f = fopen(...)) == NULL) return 0`).
/// OpenSSH's first-match-wins precedence is honoured: once a directive is
/// set inside the first matching block, a later block cannot overwrite it.
/// Wildcards in `Host` patterns are expanded with [`pattern_matches`].
///
/// # Errors
///
/// [`SshError::SshConfig`] when a line is one upstream would refuse; see
/// the module docs for the three cases.
#[cfg(test)]
pub(super) fn resolve_host(path: &Path, host_alias: &str) -> Result<ResolvedHost, SshError> {
    resolve_host_with_remote_user(path, host_alias, "")
}

/// `resolve_host` with the remote user the caller resolved from the operand
/// (`user@host`/`-l`) or a `User` directive, so a `Match user` line gates on
/// the same value `ssh` would see.
pub(super) fn resolve_host_with_remote_user(
    path: &Path,
    host_alias: &str,
    remote_user: &str,
) -> Result<ResolvedHost, SshError> {
    let mut resolved = ResolvedHost::default();
    if let Ok(text) = fs::read_to_string(path) {
        // An explicit single path behaves like upstream's `-F`: a USER
        // config (openssh/ssh.c:574 passes `SSHCONF_USERCONF`), so a
        // relative or `~`-prefixed `Include` anchors under `~/.ssh`.
        let display = path.display().to_string();
        let anchors = IncludeAnchors::live();
        let mut local_user = String::new();
        let ctx = MatchCtx::with_remote_user(host_alias, remote_user, &mut local_user);
        // First pass, then the `SSHCONF_FINAL` re-parse only when a
        // non-negated `Match final` asked for it (openssh/ssh.c:1190-1268).
        let mut want_final_pass = false;
        scan_config(
            &mut resolved,
            &text,
            &display,
            &anchors,
            true,
            false,
            0,
            true,
            &ctx,
            false,
            &mut want_final_pass,
        )?;
        if want_final_pass {
            scan_config(
                &mut resolved,
                &text,
                &display,
                &anchors,
                true,
                false,
                0,
                true,
                &ctx,
                true,
                &mut false,
            )?;
        }
    }
    Ok(resolved)
}

/// The `Include`-anchoring inputs a scan needs but a single file does not
/// carry: the home directory (for `~/` expansion), the directory a relative
/// include anchors against in a USER config (`~/.ssh`) and in a SYSTEM
/// config (`/etc/ssh`). upstream: openssh/readconf.c:2095-2104.
struct IncludeAnchors {
    home: Option<PathBuf>,
    user_dir: Option<PathBuf>,
    system_dir: PathBuf,
}

impl IncludeAnchors {
    /// The live host's anchors: `$HOME`, `$HOME/.ssh` and `/etc/ssh`.
    fn live() -> Self {
        let home = env_home();
        let user_dir = home.as_ref().map(|h| h.join(".ssh"));
        Self {
            home,
            user_dir,
            system_dir: PathBuf::from(SYSTEM_SSH_DIR),
        }
    }

    /// Anchors with no home directory, for text-only fixtures that never
    /// exercise a relative or `~`-prefixed include.
    #[cfg(test)]
    fn none() -> Self {
        Self {
            home: None,
            user_dir: None,
            system_dir: PathBuf::from(SYSTEM_SSH_DIR),
        }
    }
}

/// Resolves `host_alias` across the ordered config-file load order,
/// threading ONE [`ResolvedHost`] through every file so the per-keyword
/// claim state - first-obtained for the scalar slots, accumulating for
/// `IdentityFile` - carries across the file boundary, exactly as
/// upstream reads the user file and then the system file into the same
/// options struct (openssh/ssh.c:561-592 `process_config_files()`).
/// `Host` block state, by contrast, is per file: each scan starts back
/// at the top level (openssh/readconf.c `read_config_file_depth()`
/// re-initialises `active`).
///
/// A file flagged `check_perm` (the default `~/.ssh/config`,
/// openssh/ssh.c:583) is refused when group/world-writable or owned by
/// neither root nor the caller; a missing or unreadable file is skipped.
///
/// # Errors
///
/// [`SshError::SshConfigPermissions`] when a `check_perm` file fails
/// upstream's owner/permission check (openssh/readconf.c:2579-2587);
/// [`SshError::SshConfig`] when a line is one upstream would refuse.
/// Either aborts the load - later files are not read.
#[cfg(test)]
pub(super) fn resolve_host_files(
    files: &[ConfigFile],
    host_alias: &str,
) -> Result<ResolvedHost, SshError> {
    let mut local_user = String::new();
    let ctx = MatchCtx::live(host_alias, &mut local_user);
    resolve_host_files_with_ctx(files, &ctx, &IncludeAnchors::live())
}

/// `resolve_host_files` with the remote user the caller resolved from the
/// operand (`user@host`/`-l`) or a `User` directive, so a `Match user` line
/// gates on the same value `ssh` would see.
pub(super) fn resolve_host_files_with_remote_user(
    files: &[ConfigFile],
    host_alias: &str,
    remote_user: &str,
) -> Result<ResolvedHost, SshError> {
    let mut local_user = String::new();
    let ctx = MatchCtx::with_remote_user(host_alias, remote_user, &mut local_user);
    resolve_host_files_with_ctx(files, &ctx, &IncludeAnchors::live())
}

/// The composition behind [`resolve_host_files`], with the `Include`
/// anchors injected so tests can point them at fixture directories instead
/// of the host's real `~/.ssh` and `/etc/ssh`.
#[cfg(test)]
fn resolve_host_files_with_anchors(
    files: &[ConfigFile],
    host_alias: &str,
    anchors: &IncludeAnchors,
) -> Result<ResolvedHost, SshError> {
    let mut local_user = String::new();
    let ctx = MatchCtx::live(host_alias, &mut local_user);
    resolve_host_files_with_ctx(files, &ctx, anchors)
}

/// The shared core taking the already-built [`MatchCtx`] and anchors.
fn resolve_host_files_with_ctx(
    files: &[ConfigFile],
    ctx: &MatchCtx<'_>,
    anchors: &IncludeAnchors,
) -> Result<ResolvedHost, SshError> {
    let mut resolved = ResolvedHost::default();
    // First pass over the whole load order, then the `SSHCONF_FINAL`
    // re-parse only when a non-negated `Match final` asked for it. Upstream
    // calls `process_config_files` a second time with the SAME options
    // struct, so first-obtained-wins carries across both passes and the
    // re-parse only fills slots a `Match final`/`Match canonical` block
    // could not reach on the first (openssh/ssh.c:1190-1268).
    let want_final_pass = scan_files_once(&mut resolved, files, ctx, anchors, false)?;
    if want_final_pass {
        scan_files_once(&mut resolved, files, ctx, anchors, true)?;
    }
    Ok(resolved)
}

/// One pass over the ordered load order, threading `resolved` through every
/// file. Returns whether any file requested the final pass. `final_pass`
/// selects which of upstream's two passes this is.
fn scan_files_once(
    resolved: &mut ResolvedHost,
    files: &[ConfigFile],
    ctx: &MatchCtx<'_>,
    anchors: &IncludeAnchors,
    final_pass: bool,
) -> Result<bool, SshError> {
    let mut want_final_pass = false;
    for file in files {
        if file.check_perm && check_default_user_config_perms(&file.path).is_err() {
            return Err(SshError::SshConfigPermissions {
                path: file.path.display().to_string(),
            });
        }
        let Ok(text) = fs::read_to_string(&file.path) else {
            continue;
        };
        scan_config(
            resolved,
            &text,
            &file.path.display().to_string(),
            anchors,
            true,
            false,
            0,
            file.user_conf,
            ctx,
            final_pass,
            &mut want_final_pass,
        )?;
    }
    Ok(want_final_pass)
}

/// Variant of [`resolve_host`] that takes the config text directly.
/// Exposed for unit tests and for the `ssh -G` differential harness so
/// neither has to touch the filesystem. Refusals name
/// [`INLINE_CONFIG_NAME`] instead of a real path.
///
/// # Errors
///
/// Same as [`resolve_host`].
#[cfg(test)]
pub(super) fn resolve_host_str(text: &str, host_alias: &str) -> Result<ResolvedHost, SshError> {
    let mut local_user = String::new();
    let ctx = MatchCtx::live(host_alias, &mut local_user);
    resolve_host_str_with_ctx(text, &ctx)
}

/// [`resolve_host_str`] with an explicit remote and local user, so tests can
/// exercise `Match user`/`Match localuser` deterministically without
/// touching the process environment.
#[cfg(test)]
pub(super) fn resolve_host_str_with_users(
    text: &str,
    host_alias: &str,
    remote_user: &str,
    local_user: &str,
) -> Result<ResolvedHost, SshError> {
    let ctx = MatchCtx {
        host_alias,
        remote_user,
        local_user,
    };
    resolve_host_str_with_ctx(text, &ctx)
}

#[cfg(test)]
fn resolve_host_str_with_ctx(text: &str, ctx: &MatchCtx<'_>) -> Result<ResolvedHost, SshError> {
    let mut resolved = ResolvedHost::default();
    let anchors = IncludeAnchors::none();
    let mut want_final_pass = false;
    scan_config(
        &mut resolved,
        text,
        INLINE_CONFIG_NAME,
        &anchors,
        true,
        false,
        0,
        true,
        ctx,
        false,
        &mut want_final_pass,
    )?;
    if want_final_pass {
        scan_config(
            &mut resolved,
            text,
            INLINE_CONFIG_NAME,
            &anchors,
            true,
            false,
            0,
            true,
            ctx,
            true,
            &mut false,
        )?;
    }
    Ok(resolved)
}

/// Builds the refusal upstream would print for `path` line `line`.
fn refuse(path: &str, line: usize, reason: impl Into<String>) -> SshError {
    SshError::SshConfig {
        path: path.to_owned(),
        line,
        reason: reason.into(),
    }
}

/// The connection-derived inputs a `Match` line evaluates against, threaded
/// unchanged through the whole scan so every criterion reads the same values
/// `ssh` would have resolved once at option-processing time.
///
/// `host_alias` is the target as typed on the command line: the `Host`
/// pattern target and the `Match originalhost` input (openssh/readconf.c:1171,
/// where the criterion matches against `original_host`). `Match host` reads
/// the resolved `HostName` instead, so the alias alone does not suffice.
///
/// `remote_user` and `local_user` back `Match user` and `Match localuser`.
/// Upstream matches `user` against `ruser` - `options->user` when a `User`
/// directive or `-l`/`user@host` set one, otherwise the local `pw->pw_name`
/// (openssh/readconf.c ~1090) - and `localuser` against `pw->pw_name`
/// unconditionally (openssh/readconf.c:1182). We mirror that default in
/// [`MatchCtx::ruser`].
#[derive(Clone, Copy)]
struct MatchCtx<'a> {
    host_alias: &'a str,
    remote_user: &'a str,
    local_user: &'a str,
}

impl<'a> MatchCtx<'a> {
    /// A context for the passive resolvers, whose only connection input is
    /// the alias: no remote user was supplied, and the local user is read
    /// from the platform's canonical env var.
    #[cfg(test)]
    fn live(host_alias: &'a str, local_user_buf: &'a mut String) -> Self {
        if let Some(value) = local_user_env() {
            *local_user_buf = value;
        }
        Self {
            host_alias,
            remote_user: "",
            local_user: local_user_buf.as_str(),
        }
    }

    /// The production context: the remote user the caller resolved from the
    /// operand (`user@host`/`-l`) or a `User` directive, plus the env local
    /// user. An empty `remote_user` means none was supplied.
    fn with_remote_user(
        host_alias: &'a str,
        remote_user: &'a str,
        local_user_buf: &'a mut String,
    ) -> Self {
        if let Some(value) = local_user_env() {
            *local_user_buf = value;
        }
        Self {
            host_alias,
            remote_user,
            local_user: local_user_buf.as_str(),
        }
    }

    /// The user `Match user` matches against: the supplied remote user, or
    /// the local user when none was given, mirroring upstream's `ruser`
    /// default (openssh/readconf.c ~1090 `ruser = options->user ? ... :
    /// pw->pw_name`).
    fn ruser(&self) -> &str {
        if self.remote_user.is_empty() {
            self.local_user
        } else {
            self.remote_user
        }
    }
}

/// The local username from `USER` (Unix) or `USERNAME` (Windows), or the
/// empty string when neither is set or usable. Mirrors the `pw->pw_name`
/// source `Match localuser` reads (openssh/readconf.c:1182); reading it via
/// `std::env` keeps this crate free of the `getpwuid` FFI.
fn local_user_env() -> Option<String> {
    #[cfg(unix)]
    let raw = std::env::var_os("USER");
    #[cfg(windows)]
    let raw = std::env::var_os("USERNAME");
    #[cfg(not(any(unix, windows)))]
    let raw: Option<std::ffi::OsString> = None;

    let value = raw?.to_string_lossy().into_owned();
    if value.is_empty() { None } else { Some(value) }
}

/// Scans one file's text into `resolved`, claiming slots per the option
/// table's `ResolutionPolicy` - which is what lets a later file's scan
/// continue the same state.
///
/// `active` is the block-activity state this file is entered under: `true`
/// for a top-level file (upstream initialises `active` to 1 per file,
/// openssh/readconf.c:2556), or the containing file's active state at the
/// point of an `Include`. A `Host`/`Match` line then reassigns it. `Host`
/// block state is otherwise local to the call.
///
/// `never_match` forces every `Host`/`Match` block inactive for the whole
/// file, mirroring `SSHCONF_NEVERMATCH` (openssh/readconf.c:1841, :1882):
/// upstream sets it on an `Include` recursed while the containing block was
/// inactive, so the included file is parsed for refusals but assigns
/// nothing. `depth` is the `Include` nesting level (openssh/readconf.c:2561
/// `READCONF_MAX_DEPTH`) and `user_conf` steers where a relative `Include`
/// anchors (openssh/readconf.c:2100-2104).
///
/// `final_pass` selects which of upstream's two passes this scan is: `false`
/// for the first pass, `true` for the `SSHCONF_FINAL` re-parse
/// (openssh/ssh.c:1258-1268). It reaches [`evaluate_match_line`] so a
/// `Match final`/`Match canonical` block activates only on the final pass
/// (openssh/readconf.c:826-835). A non-negated `Match final` seen on the
/// first pass sets `want_final_pass`, which is how the driver learns it must
/// run the second pass at all (openssh/readconf.c:826-828,
/// openssh/ssh.c:1190-1192).
#[allow(clippy::too_many_arguments)]
fn scan_config(
    resolved: &mut ResolvedHost,
    text: &str,
    path: &str,
    anchors: &IncludeAnchors,
    active: bool,
    never_match: bool,
    depth: u32,
    user_conf: bool,
    ctx: &MatchCtx<'_>,
    final_pass: bool,
    want_final_pass: &mut bool,
) -> Result<(), SshError> {
    let mut in_matching_block = active;

    for (index, raw_line) in text.lines().enumerate() {
        // Upstream counts physical lines from 1 (openssh/readconf.c:2651-2654).
        let linenum = index + 1;
        // Trailing whitespace strip + leading whitespace skip
        // (openssh/readconf.c:1168-1172, :1178-1180). `\r` is in
        // upstream's WHITESPACE set (openssh/misc.c:464), so a CRLF file
        // needs no special case.
        let line = raw_line.trim();
        // A line whose keyword begins with `#` is a comment
        // (openssh/readconf.c:1181). This is the ONLY place a leading `#`
        // may be tested: mid-token a `#` is literal, and `argv_split`
        // owns that decision.
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let Some((key, value)) = split_directive(line) else {
            continue;
        };
        // One keyword lookup against the shared option table, exactly
        // where upstream resolves the opcode for the line
        // (openssh/readconf.c:1194 `parse_token`).
        let opcode = parse_token(key);

        // One tokenisation per line, exactly as upstream does it
        // (openssh/readconf.c:1196).
        let tokens =
            argv_split(value, true).map_err(|err| refuse(path, linenum, err.to_string()))?;

        if opcode == Opcode::Host {
            let matched =
                host_matches_any_pattern(ctx.host_alias, &tokens).map_err(|EmptyHostToken| {
                    // Upstream interpolates the LOWERCASED keyword it
                    // matched on (openssh/readconf.c:1184, :1833), not the
                    // spelling in the file.
                    refuse(path, linenum, "keyword host empty argument")
                })?;
            // Under `SSHCONF_NEVERMATCH` (a file included from an inactive
            // block) the block can never activate, but the tokens are still
            // walked for the empty-argument refusal above
            // (openssh/readconf.c:1841).
            in_matching_block = !never_match && matched;
            continue;
        }

        if opcode == Opcode::Match {
            // upstream: the `oMatch` arm (openssh/readconf.c:2106-2117).
            // `match_cfg_line` evaluates the criteria and the arm then sets
            // `*activep = (flags & SSHCONF_NEVERMATCH) ? 0 : value`
            // (openssh/readconf.c:2116). A bad condition returns < 0 there
            // and aborts the load regardless of the block's activity, which
            // is why the evaluation runs even under `never_match`.
            let match_host = resolved.hostname.as_deref().unwrap_or(ctx.host_alias);
            let activates = evaluate_match_line(
                &tokens,
                match_host,
                ctx,
                final_pass,
                want_final_pass,
                path,
                linenum,
            )?;
            in_matching_block = !never_match && activates;
            continue;
        }

        // `Include` is processed on every line regardless of the block's
        // activity - upstream globs and recurses unconditionally, passing
        // `SSHCONF_NEVERMATCH` down when the containing block is inactive
        // (openssh/readconf.c:2073-2150).
        if opcode == Opcode::Include {
            process_include(
                resolved,
                &tokens,
                path,
                linenum,
                anchors,
                in_matching_block,
                never_match,
                depth,
                user_conf,
                ctx,
                final_pass,
                want_final_pass,
            )?;
            continue;
        }

        // Proxy dial directives. `ProxyCommand` and `ProxyJump` consume the
        // rest of the line verbatim: upstream's `parse_command` reads
        // `s + strspn(s, WHITESPACE "=")` (openssh/readconf.c:1465-1470) and
        // `parse_jump` acts on the whole remaining string `s`
        // (openssh/readconf.c:1730-1735), neither tokenising an arg. They
        // share one first-obtained slot across both keywords - the
        // `ResolvedHost::proxy_claimed` guard mirrors upstream's mutual
        // exclusion.
        if opcode == Opcode::ProxyCommand {
            if in_matching_block && !resolved.proxy_claimed() {
                resolved.proxy_command = Some(value.to_owned());
            }
            continue;
        }
        if opcode == Opcode::ProxyJump {
            if in_matching_block && !resolved.proxy_claimed() {
                resolved.jump_hosts = Some(value.to_owned());
            }
            continue;
        }
        if opcode == Opcode::ProxyUseFdpass {
            // A multistate flag. An out-of-set token is upstream's
            // `unsupported option "%s".` (openssh/readconf.c:1269), and an
            // empty value (e.g. a lone `#comment`) is `missing argument.`
            // (openssh/readconf.c:1106) - both refused whether or not the
            // block is active, exactly as `AddressFamily` above.
            let Some(tok) = tokens.first() else {
                return Err(refuse(path, linenum, "missing argument."));
            };
            let Some(flag) = parse_flag_value(tok) else {
                return Err(refuse(
                    path,
                    linenum,
                    format!("unsupported option \"{tok}\"."),
                ));
            };
            if in_matching_block && resolved.proxy_use_fdpass.is_none() {
                resolved.proxy_use_fdpass = Some(flag);
            }
            continue;
        }

        // Every directive below is single-valued, so it consumes exactly
        // one token - upstream's `arg = argv_next(&ac, &av)`. A `NULL`
        // there is fatal, which is how `Port #comment` is refused. The
        // refusal fires BEFORE the block-activity gate: upstream
        // processes every line and `*activep` gates only the assignment
        // (e.g. openssh/readconf.c:1229 `if (*activep && *intptr == -1)`),
        // so a bad line inside a NON-matching `Host` block still aborts
        // the load. Measured on `ssh -G`: `Host other` + `Port #x` is
        // `Missing argument.` even for an alias `other` never matches.
        //
        // The opcode set is this reader's, not the table's: `Compression`
        // is in the table with the same shape but belongs to the other
        // reader, which never refuses. Widening the refusal to every known
        // keyword is upstream's behaviour and a separate row.
        let arg = match opcode {
            Opcode::Hostname
            | Opcode::User
            | Opcode::Port
            | Opcode::IdentityFile
            | Opcode::IdentitiesOnly
            | Opcode::IdentityAgent
            | Opcode::ConnectTimeout
            | Opcode::AddressFamily
            | Opcode::ServerAliveInterval
            | Opcode::ServerAliveCountMax => {
                let Some(arg) = tokens.first() else {
                    let Some(reason) = opcode.missing_argument() else {
                        continue;
                    };
                    return Err(refuse(path, linenum, reason));
                };
                arg.as_str()
            }
            // Not a directive this parser acts on. Upstream would still
            // classify it, but oc's unknown-keyword policy is task 237j's
            // row, not this one.
            _ => continue,
        };

        // Value validation that upstream performs before the `*activep`
        // check, so it too refuses from an inactive block.
        // `ConnectTimeout` and `ServerAliveInterval` share upstream's single
        // `parse_time` arm, `none`-collapse included
        // (openssh/readconf.c:1214-1228, :1916).
        let time_value = match opcode {
            Opcode::ConnectTimeout | Opcode::ServerAliveInterval => {
                parse_connect_timeout(arg).map_err(|reason| refuse(path, linenum, reason))?
            }
            _ => None,
        };
        // `AddressFamily` (a multistate): an out-of-set token is upstream's
        // `unsupported option "%s".` (openssh/readconf.c:1269).
        let address_family =
            match opcode {
                Opcode::AddressFamily => Some(parse_address_family(arg).ok_or_else(|| {
                    refuse(path, linenum, format!("unsupported option \"{arg}\"."))
                })?),
                _ => None,
            };
        // `ServerAliveCountMax` (a `parse_int`): an out-of-band or non-decimal
        // value is upstream's `integer value <errstr>.`
        // (openssh/readconf.c:1579).
        let server_alive_count_max = match opcode {
            Opcode::ServerAliveCountMax => {
                Some(parse_int_value(arg).map_err(|reason| refuse(path, linenum, reason))?)
            }
            _ => None,
        };

        if !in_matching_block {
            continue;
        }

        match opcode {
            Opcode::Hostname => set_if_unset(&mut resolved.hostname, arg.to_owned()),
            Opcode::User => set_if_unset(&mut resolved.user, arg.to_owned()),
            Opcode::Port => {
                if resolved.port.is_none()
                    && let Ok(parsed) = arg.parse::<u16>()
                {
                    resolved.port = Some(parsed);
                }
            }
            Opcode::IdentityFile => {
                let expanded = expand_tilde(arg);
                if !resolved.identity_files.contains(&expanded) {
                    resolved.identity_files.push(expanded);
                }
            }
            Opcode::IdentitiesOnly => {
                if resolved.identities_only.is_none() {
                    resolved.identities_only = parse_flag_value(arg);
                }
            }
            Opcode::IdentityAgent => {
                set_if_unset(&mut resolved.identity_agent, expand_tilde_str(arg));
            }
            Opcode::ConnectTimeout => {
                // `None` here is `ConnectTimeout none`: upstream writes -1,
                // the unset sentinel, so the slot stays claimable and a
                // LATER numeric directive still wins (measured on
                // `ssh -G`: `none` then `5` dumps 5). A plain
                // first-obtained-wins that let `none` claim the slot
                // would diverge exactly there.
                if let Some(secs) = time_value {
                    set_if_unset(&mut resolved.connect_timeout, secs);
                }
            }
            // Same `parse_time` `none`-sentinel behaviour as `ConnectTimeout`
            // above: `none` leaves the slot claimable for a later numeric.
            Opcode::ServerAliveInterval => {
                if let Some(secs) = time_value {
                    set_if_unset(&mut resolved.server_alive_interval, secs);
                }
            }
            Opcode::ServerAliveCountMax => {
                if let Some(count) = server_alive_count_max {
                    set_if_unset(&mut resolved.server_alive_count_max, count);
                }
            }
            Opcode::AddressFamily => {
                if let Some(family) = address_family {
                    set_if_unset(&mut resolved.address_family, family);
                }
            }
            _ => unreachable!("the match above already narrowed the opcode set"),
        }
    }

    Ok(())
}

/// Handles one `Include` line: for each whitespace-separated glob pattern,
/// anchors it, expands it against the filesystem, and reads every match
/// inline as a further config file.
///
/// upstream: the `oInclude` arm (openssh/readconf.c:2073-2150). `active` is
/// the containing block's activity at this line; when it is inactive, or
/// `never_match` already holds, the recursion carries `SSHCONF_NEVERMATCH`
/// so the included file assigns nothing (openssh/readconf.c:2130).
#[allow(clippy::too_many_arguments)]
fn process_include(
    resolved: &mut ResolvedHost,
    tokens: &[String],
    path: &str,
    linenum: usize,
    anchors: &IncludeAnchors,
    active: bool,
    never_match: bool,
    depth: u32,
    user_conf: bool,
    ctx: &MatchCtx<'_>,
    final_pass: bool,
    want_final_pass: &mut bool,
) -> Result<(), SshError> {
    // Upstream fatals when a file would be entered above the depth ceiling
    // (openssh/readconf.c:2573-2574). A top-level file is depth 0; this
    // include's children enter at depth+1.
    if depth + 1 > READCONF_MAX_DEPTH {
        return Err(refuse(
            path,
            linenum,
            "Too many recursive configuration includes",
        ));
    }
    let child_depth = depth + 1;
    // The recursion is NEVERMATCH when the containing block was inactive, or
    // when it already was (openssh/readconf.c:2130). The child starts at the
    // containing block's active state, which upstream shares through
    // `*activep` and restores after each file (openssh/readconf.c:2144).
    let child_never_match = never_match || !active;

    for token in tokens {
        if token.is_empty() {
            // upstream: openssh/readconf.c:2081-2083 `keyword %s empty
            // argument` (the keyword is the lowercased `include`).
            return Err(refuse(path, linenum, "keyword include empty argument"));
        }
        let pattern = match anchor_include(token, user_conf, anchors) {
            Ok(Some(pattern)) => pattern,
            // No anchor is available (e.g. no home directory for a `~/` or
            // relative user-config include): the glob matches nothing, which
            // upstream tolerates.
            Ok(None) => continue,
            Err(reason) => return Err(refuse(path, linenum, reason)),
        };
        for matched in glob_paths(&pattern) {
            // Every included file is permission-checked - upstream adds
            // `SSHCONF_CHECKPERM` to the recursion (openssh/readconf.c:2129).
            if check_default_user_config_perms(&matched).is_err() {
                return Err(SshError::SshConfigPermissions {
                    path: matched.display().to_string(),
                });
            }
            let text = match fs::read_to_string(&matched) {
                Ok(text) => text,
                // A file that vanished between the glob and the open is
                // tolerated (upstream's `errno == ENOENT` arm,
                // openssh/readconf.c:2136); present-but-unreadable is fatal.
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
                Err(err) => {
                    return Err(refuse(
                        path,
                        linenum,
                        format!("Can't open user config file {}: {err}", matched.display()),
                    ));
                }
            };
            scan_config(
                resolved,
                &text,
                &matched.display().to_string(),
                anchors,
                active,
                child_never_match,
                child_depth,
                user_conf,
                ctx,
                final_pass,
                want_final_pass,
            )?;
        }
    }
    Ok(())
}

/// Anchors one `Include` argument, returning the glob pattern to expand,
/// `None` when no anchor is available (so it matches nothing), or the
/// upstream refusal reason for a `~`-prefixed path in a system config.
///
/// upstream: openssh/readconf.c:2095-2104. A `~`-prefixed path is legal only
/// in a user config; an absolute path is taken as written; a relative path
/// anchors under `~/.ssh` (user) or `/etc/ssh` (system).
fn anchor_include(
    token: &str,
    user_conf: bool,
    anchors: &IncludeAnchors,
) -> Result<Option<String>, String> {
    if let Some(rest) = token.strip_prefix('~') {
        if !user_conf {
            return Err(format!("bad include path {token}."));
        }
        // Only the `~/` form is expanded, mirroring glob's `GLOB_TILDE`
        // against the caller's home; `~user` is left for the OS and simply
        // matches nothing here.
        let (Some(after), Some(home)) = (rest.strip_prefix('/'), anchors.home.as_deref()) else {
            return Ok(None);
        };
        return Ok(Some(format!("{}/{after}", home.display())));
    }
    if Path::new(token).is_absolute() {
        return Ok(Some(token.to_owned()));
    }
    let base = if user_conf {
        anchors.user_dir.as_deref()
    } else {
        Some(anchors.system_dir.as_path())
    };
    Ok(base.map(|dir| format!("{}/{token}", dir.display())))
}

/// Expands `pattern` - an absolute, already `~`-resolved path that may hold
/// `*` or `?` in any component - against the filesystem, returning existing
/// matches in sorted (glob) order.
///
/// Mirrors glob(3): `*`/`?` are matched per component with the shared
/// [`glob_matches`], a leading `.` is matched only by a pattern component
/// that itself starts with `.`, an unreadable directory contributes no
/// matches, and a purely-literal pattern that names nothing yields an empty
/// result - upstream's `GLOB_NOMATCH`, which the caller tolerates
/// (openssh/readconf.c:2122-2126). Character classes and `**` are not
/// supported, matching oc's existing `Host`-pattern glob scope.
fn glob_paths(pattern: &str) -> Vec<PathBuf> {
    let mut segments = pattern.split('/');
    // A leading '/' yields an empty first segment: anchor at the root.
    let first = segments.next().unwrap_or("");
    let mut bases: Vec<PathBuf> = vec![PathBuf::from(if first.is_empty() { "/" } else { first })];

    for seg in segments {
        if seg.is_empty() {
            continue; // collapse `//`
        }
        if seg.contains('*') || seg.contains('?') {
            let mut next = Vec::new();
            for base in &bases {
                let Ok(entries) = fs::read_dir(base) else {
                    continue;
                };
                for entry in entries.flatten() {
                    let name = entry.file_name();
                    let name = name.to_string_lossy();
                    // glob(3)'s leading-dot rule: `*`/`?` do not match a name
                    // beginning with `.` unless the pattern component does too.
                    if name.starts_with('.') && !seg.starts_with('.') {
                        continue;
                    }
                    if glob_matches(name.as_bytes(), seg.as_bytes()) {
                        next.push(base.join(&*name));
                    }
                }
            }
            bases = next;
        } else {
            for base in &mut bases {
                *base = base.join(seg);
            }
        }
    }

    // glob returns only paths that exist; a dangling symlink still exists as
    // a link (`lstat`), matching glob's behaviour.
    let mut out: Vec<PathBuf> = bases
        .into_iter()
        .filter(|p| p.symlink_metadata().is_ok())
        .collect();
    out.sort();
    out
}

/// Parses a `ConnectTimeout` value per the `parse_time` arm
/// (openssh/readconf.c:1215-1233). `Ok(None)` is the case-SENSITIVE
/// literal `none` (strcmp at openssh/readconf.c:1222), which upstream maps
/// onto the unset sentinel; everything else must satisfy `convtime` or the
/// load is refused with upstream's own wording. An empty token shares the
/// missing-value wording (`!arg || *arg == '\0'`, openssh/readconf.c:1218).
fn parse_connect_timeout(arg: &str) -> Result<Option<u32>, &'static str> {
    if arg.is_empty() {
        return Err("missing time value.");
    }
    if arg == "none" {
        return Ok(None);
    }
    match parse_time_value(arg) {
        Some(secs) => Ok(Some(secs)),
        None => Err("invalid time value."),
    }
}

/// A `Host` line carried a token that tokenised to the empty string.
///
/// A unit-like error rather than `()` so the call site reads as a named
/// condition and clippy's `result_unit_err` has nothing to complain about.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
struct EmptyHostToken;

/// Returns `true` when `host` matches any token in `tokens`, after
/// expanding wildcards and applying negations. A leading `!` on any token
/// negates and causes the whole line to fail to match.
///
/// Mirrors the `oHost` arm's own loop (openssh/readconf.c:1829-1858) step
/// for step, and the ORDER matters:
///
/// 1. `*activep = 0` before the loop (:1829) - a `Host` line with no
///    tokens at all leaves the block inactive.
/// 2. the empty-token guard (:1832-1836) runs FIRST, before negation is
///    stripped, so `Host a "" b` is refused even though `a` would match.
/// 3. a NEGATED match consumes the rest of the line and breaks
///    (:1845-1852), so a later empty token after it is never reached.
/// 4. a POSITIVE match does NOT break (:1854-1856): scanning continues so
///    a later negation can still kill the block.
///
/// # Errors
///
/// [`EmptyHostToken`] when a token is empty; the caller renders
/// upstream's `keyword host empty argument`.
fn host_matches_any_pattern(host: &str, tokens: &[String]) -> Result<bool, EmptyHostToken> {
    let mut any_positive_match = false;
    for token in tokens {
        if token.is_empty() {
            return Err(EmptyHostToken);
        }
        let (negate, pat) = token
            .strip_prefix('!')
            .map_or((false, token.as_str()), |stripped| (true, stripped));
        if pattern_matches(host, pat) {
            if negate {
                return Ok(false);
            }
            any_positive_match = true;
        }
    }
    Ok(any_positive_match)
}

/// Glob-matches `host` against `pattern` with no case folding, which is
/// what the `oHost` arm does (openssh/readconf.c:1844 calls `match_pattern`
/// directly). The glob itself is the shared `match_pattern` port.
fn pattern_matches(host: &str, pattern: &str) -> bool {
    glob_matches(host.as_bytes(), pattern.as_bytes())
}

/// Evaluates one `Match` line's criteria, returning whether the block
/// activates - upstream's `match_cfg_line` `result` (1 => active, 0 =>
/// inactive; a bad line returns < 0, mapped here to [`SshError`]).
///
/// Mirrors `match_cfg_line` (openssh/readconf.c:768-1045) and the two-pass
/// model it participates in (openssh/ssh.c:1190-1268):
///
/// - Criteria are AND-ed: once one fails, `result` drops to `false` and
///   never rises again, exactly as upstream only ever assigns
///   `this_result = result = 0` (openssh/readconf.c:889 onward).
/// - `all` must appear alone and takes no argument
///   (openssh/readconf.c:807-822); a leading `!` negates it.
/// - `canonical`/`final` take no argument and evaluate to the pass flag:
///   `r = !!final_pass` (openssh/readconf.c:832), so they match only on the
///   final pass. A non-negated `final` sets `want_final_pass` so the driver
///   runs that second pass (openssh/readconf.c:826-828). With
///   canonicalization unbuilt (owned by task 1218) the only trigger for the
///   final pass is a `Match final`, which is upstream's behaviour with
///   canonicalization off (openssh/ssh.c:1252-1256 leaves `want_final_pass`
///   untouched when `canonicalize_hostname` is 0).
/// - `host` matches case-INSENSITIVELY against the resolved hostname (or the
///   alias when no `HostName` has been obtained), mirroring
///   `match_hostname` (openssh/match.c:193-203).
/// - `originalhost` matches case-INSENSITIVELY against the alias as typed on
///   the command line, before any `HostName` rewrite
///   (openssh/readconf.c:1171-1174, `match_hostname(original_host, arg)`).
/// - `user` matches case-SENSITIVELY against `ruser` - the remote user the
///   operand or a `User` directive set, defaulting to the local user when
///   unset (openssh/readconf.c:1175-1178, `match_pattern_list(ruser, arg, 0)`
///   with the trailing 0 disabling case folding).
/// - `localuser` matches case-SENSITIVELY against the local user
///   (openssh/readconf.c:1180-1183, `match_pattern_list(pw->pw_name, arg, 0)`).
/// - `exec` runs its argument through the user's shell and matches when the
///   command exits 0 (openssh/readconf.c:1222-1242: `execute_in_shell(cmd)`
///   then `r = r == 0`). Negation inverts that verdict.
///
/// The criteria oc cannot yet evaluate faithfully - `localnetwork` (needs the
/// interface-address enumeration `check_match_ifaddrs`, openssh/readconf.c:1189,
/// which lives behind FFI this crate forbids), `version` (matches OpenSSH's
/// `SSH_RELEASE`, openssh/readconf.c:1196, a value oc has no analogue for),
/// `tagged` (the `Tag` directive is not modelled), and the session-only
/// `command`/`sessiontype` (openssh/readconf.c:1207-1221, no counterpart in a
/// non-interactive transfer) - keep upstream's argument grammar (a missing
/// argument refuses, openssh/readconf.c:855-858) but resolve to a non-match,
/// leaving the block inactive rather than applying options oc cannot gate
/// correctly.
fn evaluate_match_line(
    tokens: &[String],
    match_host: &str,
    ctx: &MatchCtx<'_>,
    final_pass: bool,
    want_final_pass: &mut bool,
    path: &str,
    linenum: usize,
) -> Result<bool, SshError> {
    let mut result = true;
    let mut attributes = 0usize;
    let mut it = tokens.iter().peekable();
    while let Some(raw) = it.next() {
        // A `#` token ends the criteria list (openssh/readconf.c:797-800).
        if raw.starts_with('#') {
            break;
        }
        let (negate, attrib_full) = raw
            .strip_prefix('!')
            .map_or((false, raw.as_str()), |stripped| (true, stripped));
        // An inline `attrib=value` splits here; the spaced `attrib value`
        // form consumes the next token below (openssh/readconf.c:846-853).
        let (attrib, inline_arg) = match attrib_full.split_once('=') {
            Some((keyword, value)) => (keyword, Some(value)),
            None => (attrib_full, None),
        };
        let attrib_lc = attrib.to_ascii_lowercase();

        // `all`: no argument, must appear alone (openssh/readconf.c:807-822).
        if attrib_lc == "all" {
            let trailing = it
                .peek()
                .is_some_and(|next| !next.starts_with('#') && !next.is_empty());
            if attributes > 0 || inline_arg.is_some() || trailing {
                return Err(refuse(
                    path,
                    linenum,
                    format!("'{attrib_full}' cannot be combined with other Match attributes"),
                ));
            }
            if result {
                result = !negate;
            }
            return Ok(result);
        }
        attributes += 1;

        // `canonical`/`final`: no argument; the pass flag is the predicate
        // (openssh/readconf.c:823-836).
        if attrib_lc == "canonical" || attrib_lc == "final" {
            if attrib_lc == "final" && !negate {
                *want_final_pass = true;
            }
            let matched = if negate { !final_pass } else { final_pass };
            if !matched {
                result = false;
            }
            continue;
        }

        // Every remaining criterion requires an argument
        // (openssh/readconf.c:855-872).
        let arg = match inline_arg {
            Some(arg) => arg,
            None => match it.next() {
                Some(arg) => arg.as_str(),
                None => {
                    return Err(refuse(
                        path,
                        linenum,
                        format!("missing argument for Match '{attrib}'"),
                    ));
                }
            },
        };
        if arg.is_empty() || arg.starts_with('#') {
            return Err(refuse(
                path,
                linenum,
                format!("Missing Match criteria for {attrib}"),
            ));
        }

        match attrib_lc.as_str() {
            "host" => {
                let matched = match_pattern_list_ci(match_host, arg);
                let pass = if negate { !matched } else { matched };
                if !pass {
                    result = false;
                }
            }
            // `match_hostname(original_host, arg)` (openssh/readconf.c:1171):
            // the alias as typed, before any `HostName` rewrite, matched
            // case-insensitively like `host`.
            "originalhost" => {
                let matched = match_pattern_list_ci(ctx.host_alias, arg);
                let pass = if negate { !matched } else { matched };
                if !pass {
                    result = false;
                }
            }
            // `match_pattern_list(ruser, arg, 0)` (openssh/readconf.c:1177):
            // the remote user, case-sensitive, defaulting to the local user
            // when the operand supplied none.
            "user" => {
                let matched = match_pattern_list_cs(ctx.ruser(), arg);
                let pass = if negate { !matched } else { matched };
                if !pass {
                    result = false;
                }
            }
            // `match_pattern_list(pw->pw_name, arg, 0)`
            // (openssh/readconf.c:1182): the local user, case-sensitive.
            "localuser" => {
                let matched = match_pattern_list_cs(ctx.local_user, arg);
                let pass = if negate { !matched } else { matched };
                if !pass {
                    result = false;
                }
            }
            // `execute_in_shell(cmd)` then `r = r == 0`
            // (openssh/readconf.c:1222-1242): the command runs through the
            // user's shell and a zero exit is a match; negation inverts it.
            "exec" => {
                let matched = run_match_exec(arg);
                let pass = if negate { !matched } else { matched };
                if !pass {
                    result = false;
                }
            }
            // Deferred: upstream's argument grammar is honoured above, but oc
            // has no faithful predicate (see the function's doc comment), so
            // the criterion resolves to a non-match and the block stays
            // inactive rather than mis-applied.
            "localnetwork" | "version" | "tagged" | "command" | "sessiontype" => {
                result = false;
            }
            _ => {
                // upstream: "Unsupported Match attribute" (openssh/readconf.c:1027).
                return Err(refuse(
                    path,
                    linenum,
                    format!("Unsupported Match attribute {attrib}"),
                ));
            }
        }
    }
    if attributes == 0 {
        // upstream: "One or more attributes required for Match"
        // (openssh/readconf.c:1035-1038).
        return Err(refuse(
            path,
            linenum,
            "One or more attributes required for Match",
        ));
    }
    Ok(result)
}

/// Case-INSENSITIVE glob-list match for `Match host`/`originalhost`, the
/// rule `match_hostname` applies (openssh/match.c:193-203): lowercase the
/// host and match it against a comma/whitespace-separated pattern list
/// (openssh/match.c:143) where a leading `!` on any pattern negates and a
/// negated hit fails the whole list. An empty input never matches.
fn match_pattern_list_ci(input: &str, patterns: &str) -> bool {
    if input.is_empty() {
        return false;
    }
    let input_lc = input.to_ascii_lowercase();
    let mut any_positive = false;
    for token in patterns.split(|c: char| c.is_whitespace() || c == ',') {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }
        let (negate, pattern) = token
            .strip_prefix('!')
            .map_or((false, token), |stripped| (true, stripped));
        if glob_matches(input_lc.as_bytes(), pattern.to_ascii_lowercase().as_bytes()) {
            if negate {
                return false;
            }
            any_positive = true;
        }
    }
    any_positive
}

/// Case-SENSITIVE glob-list match for `Match user`/`localuser`, the rule
/// `match_pattern_list(string, arg, 0)` applies with the trailing `0`
/// disabling case folding (openssh/readconf.c:1177, :1182; the list walker is
/// `match_pattern_list`, openssh/match.c:143-184). A leading `!` on any
/// pattern negates, and a negated hit fails the whole list. Unlike
/// [`match_pattern_list_ci`] there is no empty-input guard: upstream's
/// `ruser`/`pw_name` are never empty, and a bare `*` still matches an empty
/// string, matching `match_pattern`'s glob semantics
/// (openssh/match.c:57-114).
fn match_pattern_list_cs(input: &str, patterns: &str) -> bool {
    let mut any_positive = false;
    for token in patterns.split(|c: char| c.is_whitespace() || c == ',') {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }
        let (negate, pattern) = token
            .strip_prefix('!')
            .map_or((false, token), |stripped| (true, stripped));
        if glob_matches(input.as_bytes(), pattern.as_bytes()) {
            if negate {
                return false;
            }
            any_positive = true;
        }
    }
    any_positive
}

/// Runs a `Match exec` command through the user's shell and reports whether
/// it exited zero (a match). Mirrors `execute_in_shell`
/// (openssh/readconf.c:1500-1533): the command is passed as a single string
/// to `$SHELL -c`, falling back to `/bin/sh` when `SHELL` is unset
/// (openssh/readconf.c:1506), and only a zero exit status counts
/// (openssh/readconf.c:1237 `r = r == 0`). A shell that cannot be spawned is a
/// non-match, matching upstream's `r = 1` fallback on `waitpid`/fork failure
/// (openssh/readconf.c:1524-1528).
///
/// `std::process::Command` is a safe `std` API, so this stays inside this
/// crate's `#![deny(unsafe_code)]` without reaching for process FFI.
///
/// `%`-token expansion (`%h`, `%r`, ...) that upstream applies via
/// `expand_match_exec_or_include_path` (openssh/readconf.c:1224) is not
/// performed: the command is run verbatim. A literal command - the common
/// case - is unaffected.
#[cfg(unix)]
fn run_match_exec(command: &str) -> bool {
    use std::ffi::OsString;
    use std::process::Command;

    let shell = std::env::var_os("SHELL")
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| OsString::from("/bin/sh"));
    Command::new(shell)
        .arg("-c")
        .arg(command)
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(windows)]
fn run_match_exec(command: &str) -> bool {
    use std::ffi::OsString;
    use std::process::Command;

    let comspec = std::env::var_os("COMSPEC")
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| OsString::from("cmd.exe"));
    Command::new(comspec)
        .arg("/C")
        .arg(command)
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(not(any(unix, windows)))]
fn run_match_exec(_command: &str) -> bool {
    false
}

/// Expands a leading `~/` to the user's home directory. Returns the path
/// unchanged when expansion is not possible.
fn expand_tilde(path: &str) -> PathBuf {
    PathBuf::from(expand_tilde_str(path))
}

fn expand_tilde_str(path: &str) -> String {
    let home = home_dir();
    let home = home.as_ref().map(|h| h.to_string_lossy());
    expand_tilde_with(path, home.as_deref(), std::path::MAIN_SEPARATOR)
}

/// Expands a leading `~/` in `path` against `home`, emitting `sep` as the only
/// separator so the result never mixes `\` and `/`.
///
/// `home` and `sep` are parameters rather than reads of the environment and of
/// `MAIN_SEPARATOR` so that the Windows arm is exercisable from a unit test on
/// any host. Returns `path` unchanged when there is nothing to expand: no `~`
/// prefix, no home directory, or a `~user` form this parser does not resolve.
///
/// Mirrors OpenSSH's `tilde_expand` for the separator run that follows the
/// tilde - upstream: openssh/misc.c:1281 collapses it with
/// `copy += strspn(copy, "/")` before concatenating, which is why a value of
/// `~//probe` must still land under the home directory.
///
/// Two behaviours are Windows-specific, where OpenSSH assumes a single
/// separator character:
///
/// * `~\` is accepted as a tilde prefix when `sep` is `\`. This mirrors the
///   `WINDOWS` arm at openssh/misc.c:1284, which was added to `tilde_expand`
///   for exactly this reason.
/// * The remainder's separators are rewritten to `sep`. OpenSSH concatenates
///   with a literal `/` and leaves the remainder alone (openssh/misc.c:1315),
///   which on Windows yields a mixed path such as `C:\Users\x/.ssh/id_rsa`;
///   it gets away with it because the Win32 file APIs accept either character.
///   oc puts these paths in front of the user in diagnostics, so emitting one
///   separator kind is a deliberate oc divergence rather than a mirror.
fn expand_tilde_with(path: &str, home: Option<&str>, sep: char) -> String {
    let is_sep = |c: char| c == '/' || (sep == '\\' && c == '\\');
    let Some(rest) = path
        .strip_prefix('~')
        .and_then(|after| after.strip_prefix(is_sep))
    else {
        return path.to_owned();
    };
    let Some(home) = home else {
        return path.to_owned();
    };
    let rest = rest.trim_start_matches(is_sep);
    let home = home.trim_end_matches(is_sep);
    let mut out = String::with_capacity(home.len() + 1 + rest.len());
    out.push_str(home);
    out.push(sep);
    out.extend(rest.chars().map(|c| if is_sep(c) { sep } else { c }));
    out
}

fn home_dir() -> Option<PathBuf> {
    #[cfg(unix)]
    {
        std::env::var_os("HOME").map(PathBuf::from)
    }
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE").map(PathBuf::from)
    }
    #[cfg(not(any(unix, windows)))]
    {
        None
    }
}

fn set_if_unset<T>(slot: &mut Option<T>, value: T) {
    if slot.is_none() {
        *slot = Some(value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Resolve, asserting the config was accepted. Most cells below are
    /// about which value wins, not about refusal.
    fn resolve(text: &str, alias: &str) -> ResolvedHost {
        resolve_host_str(text, alias).expect("config accepted")
    }

    /// The `path line N: reason` text of a refusal, for the cells that
    /// assert on upstream's wording.
    fn refusal(text: &str, alias: &str) -> String {
        match resolve_host_str(text, alias) {
            Ok(resolved) => panic!("expected a refusal, resolved {resolved:?}"),
            Err(err) => err.to_string(),
        }
    }

    /// Resolve with an explicit remote and local user injected, so the
    /// `Match user`/`Match localuser` cells are deterministic regardless of
    /// the process environment.
    fn resolve_users(text: &str, alias: &str, remote_user: &str, local_user: &str) -> ResolvedHost {
        resolve_host_str_with_users(text, alias, remote_user, local_user).expect("config accepted")
    }

    #[test]
    fn missing_file_returns_default() {
        let resolved =
            resolve_host(Path::new("/nonexistent/ssh/config"), "anything").expect("absent is Ok");
        assert!(resolved.hostname.is_none());
        assert!(resolved.user.is_none());
        assert!(resolved.port.is_none());
        assert!(resolved.identity_files.is_empty());
    }

    // --- Proxy directives: ProxyCommand / ProxyJump / ProxyUseFdpass ---

    #[test]
    fn proxy_command_is_the_rest_of_the_line_verbatim() {
        // The value is taken whole, not tokenised: internal spaces and `%`
        // tokens survive for expansion at dial time.
        let resolved = resolve("Host t\n  ProxyCommand ssh -W %h:%p bastion\n", "t");
        assert_eq!(
            resolved.proxy_command.as_deref(),
            Some("ssh -W %h:%p bastion")
        );
        assert!(resolved.jump_hosts.is_none());
    }

    #[test]
    fn proxy_command_equals_form_strips_the_separator() {
        let resolved = resolve("Host t\n  ProxyCommand=nc %h %p\n", "t");
        assert_eq!(resolved.proxy_command.as_deref(), Some("nc %h %p"));
    }

    #[test]
    fn proxy_jump_is_recorded() {
        let resolved = resolve("Host t\n  ProxyJump alice@bastion:2222\n", "t");
        assert_eq!(resolved.jump_hosts.as_deref(), Some("alice@bastion:2222"));
        assert!(resolved.proxy_command.is_none());
    }

    #[test]
    fn proxy_command_and_jump_share_one_first_obtained_slot() {
        // ProxyCommand first claims the shared slot, so the later ProxyJump
        // is ignored - upstream's mutual exclusion
        // (openssh/readconf.c:1730 active-guard).
        let resolved = resolve(
            "Host t\n  ProxyCommand nc %h %p\n  ProxyJump bastion\n",
            "t",
        );
        assert_eq!(resolved.proxy_command.as_deref(), Some("nc %h %p"));
        assert!(resolved.jump_hosts.is_none());
    }

    #[test]
    fn proxy_jump_first_blocks_a_later_proxy_command() {
        // The reverse order: ProxyJump claims the slot first, so the later
        // ProxyCommand is dropped.
        let resolved = resolve(
            "Host t\n  ProxyJump bastion\n  ProxyCommand nc %h %p\n",
            "t",
        );
        assert_eq!(resolved.jump_hosts.as_deref(), Some("bastion"));
        assert!(resolved.proxy_command.is_none());
    }

    #[test]
    fn proxy_use_fdpass_parses_as_a_flag() {
        assert_eq!(
            resolve("Host t\n  ProxyUseFdpass yes\n", "t").proxy_use_fdpass,
            Some(true)
        );
        assert_eq!(
            resolve("Host t\n  ProxyUseFdpass no\n", "t").proxy_use_fdpass,
            Some(false)
        );
    }

    #[test]
    fn proxy_use_fdpass_rejects_an_out_of_set_value() {
        // A multistate flag: upstream refuses a non-yes/no token with
        // `unsupported option "%s".` (openssh/readconf.c:1269).
        let msg = refusal("Host t\n  ProxyUseFdpass maybe\n", "t");
        assert!(msg.contains("unsupported option \"maybe\"."), "{msg}");
    }

    /// Non-vacuity control for the proxy parse cells: a host the block does
    /// not match resolves none of the proxy slots, so the assertions above
    /// are exercising the matcher, not accepting every input.
    #[test]
    fn proxy_directives_ignored_outside_a_matching_block() {
        let resolved = resolve("Host other\n  ProxyCommand nc %h %p\n", "t");
        assert!(resolved.proxy_command.is_none());
        assert!(resolved.jump_hosts.is_none());
        assert!(resolved.proxy_use_fdpass.is_none());
    }

    // --- Match blocks + the two-pass model (task 237g) ---
    //
    // Oracle-independent counterparts to the `ssh -G` differential cells,
    // so the two-pass model is gated even where no `ssh` binary is present.
    // Every value here was measured against `OpenSSH_10.3p1`.

    #[test]
    fn match_all_block_applies() {
        let resolved = resolve("Match all\n  Port 2266\n", "t");
        assert_eq!(resolved.port, Some(2266));
    }

    #[test]
    fn negated_match_all_never_applies() {
        // `Match !all` is the never-match sentinel: result = negate ? 0 : 1.
        let resolved = resolve("Match !all\n  Port 2266\n", "t");
        assert_eq!(resolved.port, None);
    }

    #[test]
    fn match_final_activates_only_on_the_second_pass() {
        // Single-pass resolution would leave the port unset; the block
        // applies only because the driver runs the SSHCONF_FINAL re-parse.
        let resolved = resolve("Match final\n  Port 2244\n  User finaluser\n", "t");
        assert_eq!(resolved.port, Some(2244));
        assert_eq!(resolved.user.as_deref(), Some("finaluser"));
    }

    #[test]
    fn match_canonical_alone_stays_inactive() {
        // `canonical` never sets want_final_pass, and with canonicalization
        // off there is no second pass, so the block never activates.
        let resolved = resolve("Match canonical\n  Port 2255\n", "t");
        assert_eq!(resolved.port, None);
    }

    #[test]
    fn negated_match_final_matches_on_the_first_pass_only() {
        // `!final` matches when !final_pass and does not request a second
        // pass, so it applies on the first pass and there is no re-parse.
        let resolved = resolve("Match !final\n  Port 2233\n", "t");
        assert_eq!(resolved.port, Some(2233));
    }

    #[test]
    fn first_obtained_wins_across_the_two_passes() {
        let resolved = resolve("Port 2001\nMatch final\n  Port 2002\n", "t");
        assert_eq!(resolved.port, Some(2001));
        // Control: with no first-obtained value the final block claims it.
        let control = resolve("Match final\n  Port 2002\n", "t");
        assert_eq!(control.port, Some(2002));
    }

    #[test]
    fn match_host_gates_case_insensitively_and_globs() {
        let matched = resolve(
            "Match host prod-*.example.com\n  Port 2277\n",
            "PROD-web1.example.com",
        );
        assert_eq!(matched.port, Some(2277));
        let declined = resolve(
            "Match host prod-*.example.com\n  Port 2277\n",
            "dev-web1.example.com",
        );
        assert_eq!(declined.port, None);
    }

    #[test]
    fn negated_match_host_inverts_the_gate() {
        let allowed = resolve(
            "Match !host banned.example.com\n  Port 2288\n",
            "ok.example.com",
        );
        assert_eq!(allowed.port, Some(2288));
        let blocked = resolve(
            "Match !host banned.example.com\n  Port 2288\n",
            "banned.example.com",
        );
        assert_eq!(blocked.port, None);
    }

    #[test]
    fn match_host_reads_the_resolved_hostname_when_obtained() {
        // A first-obtained `HostName` becomes the `Match host` input, so the
        // block gates on the resolved name rather than the alias.
        let resolved = resolve(
            "Host t\n  HostName real.example.com\nMatch host real.example.com\n  Port 2299\n",
            "t",
        );
        assert_eq!(resolved.hostname.as_deref(), Some("real.example.com"));
        assert_eq!(resolved.port, Some(2299));
    }

    #[test]
    fn match_host_inline_equals_form_is_accepted() {
        let resolved = resolve("Match host=t.example.com\n  Port 2300\n", "t.example.com");
        assert_eq!(resolved.port, Some(2300));
    }

    #[test]
    fn deferred_match_criterion_leaves_the_block_inactive() {
        // `tagged` has no faithful oc predicate (no `Tag` directive): its
        // argument is validated but it resolves to a non-match, so the block
        // does not apply. Control: an otherwise identical `Match all` block
        // DOES apply, proving the inactivity is the criterion, not the
        // surrounding parse.
        let deferred = resolve("Match tagged prod\n  Port 2311\n", "t");
        assert_eq!(deferred.port, None);
        let control = resolve("Match all\n  Port 2311\n", "t");
        assert_eq!(control.port, Some(2311));
    }

    #[test]
    fn match_originalhost_gates_on_the_typed_alias() {
        // `originalhost` reads the alias as typed, before any `HostName`
        // rewrite, matched case-insensitively (openssh/readconf.c:1171).
        let hit = resolve(
            "HostName real.example\nMatch originalhost T\n  Port 2401\n",
            "t",
        );
        assert_eq!(hit.port, Some(2401));
        // Control: a non-matching alias leaves the block inactive.
        let miss = resolve("Match originalhost other\n  Port 2401\n", "t");
        assert_eq!(miss.port, None);
    }

    #[test]
    fn negated_match_originalhost_inverts_the_gate() {
        let applies = resolve("Match !originalhost other\n  Port 2402\n", "t");
        assert_eq!(applies.port, Some(2402));
        let blocked = resolve("Match !originalhost t\n  Port 2402\n", "t");
        assert_eq!(blocked.port, None);
    }

    #[test]
    fn match_user_gates_on_the_remote_user_case_sensitively() {
        // `Match user` reads the remote user, case-sensitively
        // (openssh/readconf.c:1177, dolower off).
        let hit = resolve_users("Match user deploy\n  Port 2411\n", "t", "deploy", "local");
        assert_eq!(hit.port, Some(2411));
        // Control 1: a different user does not match.
        let miss = resolve_users("Match user deploy\n  Port 2411\n", "t", "other", "local");
        assert_eq!(miss.port, None);
        // Control 2: case matters - `Deploy` != `deploy`.
        let cased = resolve_users("Match user deploy\n  Port 2411\n", "t", "Deploy", "local");
        assert_eq!(cased.port, None);
    }

    #[test]
    fn match_user_defaults_to_the_local_user_when_none_supplied() {
        // With no remote user, `ruser` defaults to the local user
        // (openssh/readconf.c ~1090).
        let hit = resolve_users("Match user localguy\n  Port 2412\n", "t", "", "localguy");
        assert_eq!(hit.port, Some(2412));
        let miss = resolve_users("Match user someone\n  Port 2412\n", "t", "", "localguy");
        assert_eq!(miss.port, None);
    }

    #[test]
    fn negated_match_user_inverts_the_gate() {
        let applies = resolve_users("Match !user root\n  Port 2413\n", "t", "deploy", "l");
        assert_eq!(applies.port, Some(2413));
        let blocked = resolve_users("Match !user deploy\n  Port 2413\n", "t", "deploy", "l");
        assert_eq!(blocked.port, None);
    }

    #[test]
    fn match_localuser_gates_on_the_local_user() {
        // `Match localuser` reads the local user, case-sensitively
        // (openssh/readconf.c:1182). It ignores the remote user: the remote
        // is `root` here, yet the block gates on the local `builder`.
        let hit = resolve_users(
            "Match localuser builder\n  Port 2421\n",
            "t",
            "root",
            "builder",
        );
        assert_eq!(hit.port, Some(2421));
        let miss = resolve_users(
            "Match localuser builder\n  Port 2421\n",
            "t",
            "root",
            "someone",
        );
        assert_eq!(miss.port, None);
    }

    #[test]
    fn match_and_conjunction_requires_every_criterion() {
        // AND across criteria: the block applies only when BOTH the user and
        // the originalhost match. This is the mutation cell - flipping the
        // AND to an OR makes the mixed rows below activate the block.
        let both = resolve_users(
            "Match user deploy originalhost t\n  Port 2431\n",
            "t",
            "deploy",
            "l",
        );
        assert_eq!(both.port, Some(2431), "both criteria true must apply");

        let user_only = resolve_users(
            "Match user deploy originalhost other\n  Port 2431\n",
            "t",
            "deploy",
            "l",
        );
        assert_eq!(
            user_only.port, None,
            "originalhost false must fail the block"
        );

        let host_only = resolve_users(
            "Match user wrong originalhost t\n  Port 2431\n",
            "t",
            "deploy",
            "l",
        );
        assert_eq!(host_only.port, None, "user false must fail the block");
    }

    #[cfg(unix)]
    #[test]
    fn match_exec_matches_on_zero_exit_only() {
        // `Match exec` runs the command through the shell; exit 0 is a match,
        // nonzero is not (openssh/readconf.c:1237 `r = r == 0`). Skip where
        // no POSIX shell is available.
        if !std::path::Path::new("/bin/sh").exists() {
            return;
        }
        let hit = resolve("Match exec \"exit 0\"\n  Port 2441\n", "t");
        assert_eq!(hit.port, Some(2441), "exit 0 must match");
        let miss = resolve("Match exec \"exit 3\"\n  Port 2441\n", "t");
        assert_eq!(miss.port, None, "nonzero exit must not match");
        // Negation inverts the verdict.
        let negated = resolve("Match !exec \"exit 3\"\n  Port 2441\n", "t");
        assert_eq!(negated.port, Some(2441), "negated nonzero exit must match");
    }

    #[test]
    fn match_user_missing_argument_is_refused() {
        // The argument grammar is upstream's: a bare criterion refuses
        // (openssh/readconf.c:855-858).
        let err = refusal("Match user\n  Port 22\n", "t");
        assert!(
            err.contains("missing argument for Match"),
            "unexpected refusal: {err}"
        );
    }

    #[test]
    fn match_host_missing_argument_is_refused() {
        let err = refusal("Match host\n  Port 22\n", "t");
        assert!(
            err.contains("missing argument for Match"),
            "unexpected refusal: {err}"
        );
    }

    #[test]
    fn match_all_combined_with_another_attribute_is_refused() {
        let err = refusal("Match all host t\n  Port 22\n", "t");
        assert!(
            err.contains("cannot be combined with other Match attributes"),
            "unexpected refusal: {err}"
        );
    }

    #[test]
    fn unsupported_match_attribute_is_refused() {
        let err = refusal("Match bogus t\n  Port 22\n", "t");
        assert!(
            err.contains("Unsupported Match attribute"),
            "unexpected refusal: {err}"
        );
    }

    #[test]
    fn match_block_ends_at_the_next_host_line() {
        // A `Host` line closes the Match block, matching upstream's
        // block-activity reset.
        let resolved = resolve("Match all\n  Port 2266\nHost other\n  User wrong\n", "t");
        assert_eq!(resolved.port, Some(2266));
        assert_eq!(resolved.user, None);
    }

    #[test]
    fn simple_host_block_applies() {
        let text = "Host example\n  HostName 1.2.3.4\n  User deploy\n  Port 2222\n";
        let resolved = resolve(text, "example");
        assert_eq!(resolved.hostname.as_deref(), Some("1.2.3.4"));
        assert_eq!(resolved.user.as_deref(), Some("deploy"));
        assert_eq!(resolved.port, Some(2222));
    }

    #[test]
    fn non_matching_block_is_ignored() {
        let text = "Host other\n  HostName ignored\n";
        assert!(resolve(text, "example").hostname.is_none());
    }

    #[test]
    fn wildcard_star_matches() {
        let text = "Host *.example.com\n  User wild\n";
        assert_eq!(
            resolve(text, "alpha.example.com").user.as_deref(),
            Some("wild")
        );
    }

    #[test]
    fn first_match_wins() {
        let text = "Host example\n  User first\nHost *\n  User second\n";
        assert_eq!(resolve(text, "example").user.as_deref(), Some("first"));
    }

    #[test]
    fn top_level_directive_applies_to_all_hosts() {
        // Directives before the first Host line sit at the always-active
        // top level and apply to every host, because upstream initialises
        // `active` to 1 (openssh/readconf.c read_config_file_depth()). The
        // Host-scoped HostName is the non-vacuity control: it must stay
        // confined to the host `Host example` names.
        let text = "User toplevel\nPort 2222\nHost example\n  HostName 1.2.3.4\n";

        let matched = resolve(text, "example");
        assert_eq!(matched.user.as_deref(), Some("toplevel"));
        assert_eq!(matched.port, Some(2222));
        assert_eq!(matched.hostname.as_deref(), Some("1.2.3.4"));

        let unmatched = resolve(text, "nomatch");
        assert_eq!(unmatched.user.as_deref(), Some("toplevel"));
        assert_eq!(unmatched.port, Some(2222));
        // Control: a directive confined to the non-matching Host block
        // never reaches a host the block did not name.
        assert!(unmatched.hostname.is_none());
    }

    #[test]
    fn top_level_directive_wins_over_later_host_block() {
        // First-obtained-wins: a top-level value claims the slot before a
        // later matching Host block can overwrite it (openssh/readconf.c
        // :1229 `if (*activep && *intptr == -1)`).
        let text = "User topuser\nHost example\n  User blockuser\n";
        assert_eq!(resolve(text, "example").user.as_deref(), Some("topuser"));
    }

    #[test]
    fn comments_and_blank_lines_skipped() {
        let text = "# top comment\n\nHost example  # inline\n  User u # trail\n";
        assert_eq!(resolve(text, "example").user.as_deref(), Some("u"));
    }

    #[test]
    fn equals_separator_supported() {
        let text = "Host example\n  Port=4242\n";
        assert_eq!(resolve(text, "example").port, Some(4242));
    }

    #[test]
    fn negation_disables_block() {
        let text = "Host *.example.com !banned.example.com\n  User u\n";
        assert!(resolve(text, "banned.example.com").user.is_none());
        assert_eq!(resolve(text, "ok.example.com").user.as_deref(), Some("u"));
    }

    /// A comma inside a `Host` token is pattern text, not a separator.
    ///
    /// Upstream tokenises the line with `argv_split`, whose separators are
    /// `' '` and `'\t'` only (openssh/misc.c:2141), and matches each token
    /// with `match_pattern` (openssh/readconf.c:1844). Measured on real
    /// `ssh -G -F fixture a`: `port 22`, i.e. no match. The differential
    /// harness pins that half against the oracle; this pins the mirror
    /// image, which the oracle cannot answer because `ssh -G a,b` refuses
    /// the alias as an invalid hostname.
    #[test]
    fn comma_in_a_host_pattern_is_literal_not_a_separator() {
        let text = "Host a,b\n  Port 2222\n";
        assert!(resolve(text, "a").port.is_none());
        assert!(resolve(text, "b").port.is_none());
        assert_eq!(resolve(text, "a,b").port, Some(2222));
    }

    /// A TAB separates `Host` tokens exactly as a space does
    /// (openssh/misc.c:2141), so narrowing the separator set to space and
    /// TAB must not have dropped the TAB.
    #[test]
    fn tab_separates_host_patterns() {
        let text = "Host first\tsecond\n  Port 2222\n";
        assert_eq!(resolve(text, "first").port, Some(2222));
        assert_eq!(resolve(text, "second").port, Some(2222));
    }

    #[test]
    fn identities_only_yes_is_recognised() {
        let text = "Host example\n  IdentitiesOnly yes\n";
        assert_eq!(resolve(text, "example").identities_only, Some(true));
    }

    #[test]
    fn identity_files_collected_in_order() {
        let text = "Host example\n  IdentityFile /a\n  IdentityFile /b\n";
        assert_eq!(
            resolve(text, "example").identity_files,
            vec![PathBuf::from("/a"), PathBuf::from("/b")]
        );
    }

    #[test]
    fn invalid_port_is_ignored() {
        let text = "Host example\n  Port notanumber\n";
        assert!(resolve(text, "example").port.is_none());
    }

    // -- argv_split boundary behaviour, as seen through the directives --
    //
    // The tokeniser itself is unit-tested in `crate::ssh::argv_split`;
    // these cells prove the parser CONSUMES those tokens rather than
    // re-deriving its own. Each was measured against real `ssh -G`
    // (OpenSSH_10.3p1) before being written down.

    /// A `#` inside a token is pattern text, so the alias that matches is
    /// `a#b` and not `a`. Before the tokeniser landed, a whole-line
    /// comment strip cut the value to `a` and inverted both rows.
    /// Oracle: `Host a#b` + alias `a#b` gives `port 2222`; alias `a` gives
    /// `port 22`.
    #[test]
    fn a_hash_inside_a_host_token_is_pattern_text() {
        let text = "Host a#b\n  Port 2222\n";
        assert_eq!(resolve(text, "a#b").port, Some(2222));
        assert!(resolve(text, "a").port.is_none());
    }

    /// The non-vacuity companion: at a token boundary the `#` really does
    /// end the line, so the trailing token is not a pattern.
    /// Oracle: `Host a #b` + alias `a` gives `port 2222`.
    #[test]
    fn a_hash_at_a_token_boundary_still_ends_the_host_line() {
        let text = "Host a #b\n  Port 2222\n";
        assert_eq!(resolve(text, "a").port, Some(2222));
        assert!(resolve(text, "b").port.is_none());
    }

    /// Quotes are consumed, not matched: `Host "a"` matches the alias `a`.
    /// Oracle: `port 2222`. Without the tokeniser oc compared the alias
    /// against the literal `"a"` and declined.
    #[test]
    fn quotes_around_a_host_token_are_stripped() {
        assert_eq!(resolve("Host \"a\"\n  Port 2222\n", "a").port, Some(2222));
        assert_eq!(resolve("Host 'a'\n  Port 2222\n", "a").port, Some(2222));
    }

    /// A quoted wildcard is still a wildcard once the quotes are gone.
    /// Oracle: `Host "*"` + alias `x` gives `port 2222`.
    #[test]
    fn a_quoted_wildcard_still_globs() {
        assert_eq!(resolve("Host \"*\"\n  Port 2222\n", "x").port, Some(2222));
    }

    /// A quote may open and close mid-token, splicing the halves.
    /// Oracle: `Host "a"b` + alias `ab` gives `port 2222`.
    #[test]
    fn a_mid_token_quote_splices_the_host_pattern() {
        assert_eq!(resolve("Host \"a\"b\n  Port 2222\n", "ab").port, Some(2222));
    }

    /// Quotes group a space into ONE pattern, so `Host "web 1" other`
    /// carries two tokens and `web` matches neither.
    /// Oracle: alias `web` gives `port 22`; alias `other` gives 2222.
    /// (`ssh -G 'web 1'` refuses the alias itself, so the grouped token is
    /// asserted here rather than differentially - the same oracle boundary
    /// task 1204 recorded for `a,b`.)
    #[test]
    fn quotes_group_a_space_into_one_host_pattern() {
        let text = "Host \"web 1\" other\n  Port 2222\n";
        assert!(resolve(text, "web").port.is_none());
        assert!(resolve(text, "1").port.is_none());
        assert_eq!(resolve(text, "other").port, Some(2222));
        assert_eq!(resolve(text, "web 1").port, Some(2222));
    }

    /// Non-vacuity companion for the row above: strip the quotes and the same
    /// line is THREE patterns, so `web` matches on its own. Without this the
    /// quoted assertions would also hold for a tokeniser that dropped the
    /// whole line.
    /// Oracle: `Host web 1 other` + alias `web` gives `port 2222`, where the
    /// quoted spelling gives `port 22`.
    #[test]
    fn a_bare_space_still_separates_two_host_patterns() {
        let text = "Host web 1 other\n  Port 2222\n";
        assert_eq!(resolve(text, "web").port, Some(2222));
        assert_eq!(resolve(text, "1").port, Some(2222));
        assert_eq!(resolve(text, "other").port, Some(2222));
    }

    /// An escaped quote is ordinary text, so the token keeps the `"`.
    /// Oracle: `Host \"a` + alias `a` gives `port 22`.
    #[test]
    fn an_escaped_quote_stays_in_the_host_pattern() {
        let text = "Host \\\"a\n  Port 2222\n";
        assert!(resolve(text, "a").port.is_none());
        assert_eq!(resolve(text, "\"a").port, Some(2222));
    }

    /// A directive VALUE keeps its `#` too - this is the row that reaches
    /// every keyword, not just `Host`.
    /// Oracle: `HostName x#y` dumps `hostname x#y`; `IdentityFile /k#1`
    /// dumps `identityfile /k#1`.
    #[test]
    fn a_hash_inside_a_directive_value_is_literal() {
        let text = "Host a\n  HostName x#y\n  IdentityFile /k#1\n";
        let resolved = resolve(text, "a");
        assert_eq!(resolved.hostname.as_deref(), Some("x#y"));
        assert_eq!(identity_file_strings(&resolved), vec!["/k#1".to_owned()]);
    }

    /// A quoted directive value keeps its embedded space and loses the
    /// quotes. Oracle: `HostName "x y"` dumps `hostname x y`.
    #[test]
    fn a_quoted_directive_value_keeps_its_space_and_loses_the_quotes() {
        let text = "Host a\n  HostName \"x y\"\n";
        assert_eq!(resolve(text, "a").hostname.as_deref(), Some("x y"));
    }

    /// An unrecognised escape keeps BOTH characters
    /// (openssh/misc.c:2161-2164). Oracle: `HostName x\ny` dumps
    /// `hostname x\ny`, and `HostName "x\ y"` dumps `hostname x\ y`
    /// because the space escape is quote-gated.
    #[test]
    fn an_unrecognised_escape_survives_into_the_value() {
        assert_eq!(
            resolve("Host a\n  HostName x\\ny\n", "a")
                .hostname
                .as_deref(),
            Some("x\\ny")
        );
        assert_eq!(
            resolve("Host a\n  HostName \"x\\ y\"\n", "a")
                .hostname
                .as_deref(),
            Some("x\\ y")
        );
    }

    // -- the refusals ---------------------------------------------------

    /// The empty-token hard error, upstream's own wording.
    /// Oracle: `ssh -G` exits 255 with
    /// `<file> line 1: keyword host empty argument`
    /// (openssh/readconf.c:1832-1836), measured for the middle, first and
    /// last positions and for both quote characters.
    #[test]
    fn an_empty_host_token_is_refused_in_every_position() {
        for text in [
            "Host a \"\" b\n  Port 2222\n",
            "Host \"\" a\n  Port 2222\n",
            "Host a \"\"\n  Port 2222\n",
            "Host a '' b\n  Port 2222\n",
        ] {
            assert_eq!(
                refusal(text, "a"),
                "<ssh_config> line 1: keyword host empty argument",
                "fixture {text:?}"
            );
        }
    }

    /// The non-vacuity companion: the same shapes WITHOUT an empty token
    /// resolve. Without this, a parser that refused every quoted `Host`
    /// line would satisfy the cell above.
    #[test]
    fn a_host_line_with_no_empty_token_is_accepted() {
        assert_eq!(
            resolve("Host a \"b\" c\n  Port 2222\n", "a").port,
            Some(2222)
        );
        assert_eq!(
            resolve("Host a \"b\" c\n  Port 2222\n", "b").port,
            Some(2222)
        );
    }

    /// A NEGATED match consumes the rest of the line, so an empty token
    /// after it is never reached (openssh/readconf.c:1845-1852). This is
    /// the ordering cell: a guard hoisted ahead of the loop would refuse
    /// here and diverge.
    #[test]
    fn an_empty_token_after_a_negated_match_is_not_reached() {
        let text = "Host !a \"\"\n  Port 2222\n";
        assert!(resolve(text, "a").port.is_none());
        // For an alias the negation does NOT match, the loop runs on and
        // the empty token IS refused.
        assert_eq!(
            refusal(text, "z"),
            "<ssh_config> line 1: keyword host empty argument"
        );
    }

    /// An unterminated quote refuses the line, with upstream's wording.
    /// Oracle: `ssh -G` exits 255 with `<file> line 1: invalid quotes`
    /// (openssh/readconf.c:1196-1199).
    #[test]
    fn an_unterminated_quote_is_refused() {
        assert_eq!(
            refusal("Host \"a b\n  Port 2222\n", "a"),
            "<ssh_config> line 1: invalid quotes"
        );
        // A quote on a VALUE line refuses too, and carries that line's
        // number - proving the refusal is per-line, not Host-specific.
        assert_eq!(
            refusal("Host a\n  HostName \"x\n", "a"),
            "<ssh_config> line 2: invalid quotes"
        );
    }

    /// A directive whose value tokenises to nothing is the
    /// `argv_next` returned NULL arm. Oracle-measured per keyword: the
    /// string options capitalise it (openssh/readconf.c:1364) and the
    /// yes/no flags do not (openssh/readconf.c:1240).
    #[test]
    fn a_value_that_is_only_a_comment_is_refused() {
        for (keyword, reason) in [
            ("HostName", "Missing argument."),
            ("User", "Missing argument."),
            ("Port", "Missing argument."),
            ("IdentityFile", "Missing argument."),
            ("IdentityAgent", "Missing argument."),
            ("IdentitiesOnly", "missing argument."),
        ] {
            let text = format!("Host a\n  {keyword} #x\n");
            assert_eq!(
                refusal(&text, "a"),
                format!("<ssh_config> line 2: {reason}"),
                "keyword {keyword}"
            );
        }
    }

    /// The non-vacuity companion for the cell above: the same directives
    /// with a real value are accepted, and a comment AFTER the value is
    /// not a missing argument.
    #[test]
    fn a_value_followed_by_a_comment_is_not_missing() {
        let text = "Host a\n  HostName real #note\n  Port 2222 #note\n";
        let resolved = resolve(text, "a");
        assert_eq!(resolved.hostname.as_deref(), Some("real"));
        assert_eq!(resolved.port, Some(2222));
    }

    /// A `Host` line whose value is only a comment leaves the block
    /// INACTIVE rather than refusing - upstream clears `*activep` before
    /// the loop and the loop then runs zero times
    /// (openssh/readconf.c:1829-1831). Oracle: `port 22`, exit 0.
    #[test]
    fn a_host_line_that_is_only_a_comment_deactivates_without_refusing() {
        let text = "Host a\n  Port 2222\nHost #x\n  Port 3333\n";
        assert_eq!(resolve(text, "a").port, Some(2222));
        assert!(resolve("Host #x\n  Port 3333\n", "a").port.is_none());
    }

    // -- ConnectTimeout ---------------------------------------------------
    //
    // Every cell below was measured against real `ssh -G` (OpenSSH_10.3p1)
    // before being written down; the differential harness pins the
    // oracle-reachable half live.

    #[test]
    fn connect_timeout_resolves_in_seconds() {
        assert_eq!(
            resolve("Host a\n  ConnectTimeout 7\n", "a").connect_timeout,
            Some(7)
        );
        // convtime's qualifier forms reach the directive.
        assert_eq!(
            resolve("Host a\n  ConnectTimeout 1m30s\n", "a").connect_timeout,
            Some(90)
        );
        // 0 is a VALUE, not unset (upstream dumps `connecttimeout 0`).
        assert_eq!(
            resolve("Host a\n  ConnectTimeout 0\n", "a").connect_timeout,
            Some(0)
        );
    }

    /// First-obtained-wins, the property the whole 237d row hangs on.
    /// Oracle: `7` then `9` dumps `connecttimeout 7`.
    #[test]
    fn connect_timeout_first_obtained_wins() {
        let text = "Host a\n  ConnectTimeout 7\n  ConnectTimeout 9\n";
        assert_eq!(resolve(text, "a").connect_timeout, Some(7));
    }

    /// The `none` SENTINEL quirk: upstream's `none` writes -1, which IS
    /// the unset sentinel, so `none` never claims the first-obtained slot
    /// and a LATER value still wins (openssh/readconf.c:1222, :1229).
    /// Oracle: `none` then `5` dumps `connecttimeout 5`; `5` then `none`
    /// dumps 5; `none` alone dumps `connecttimeout none`.
    #[test]
    fn connect_timeout_none_is_the_unset_sentinel_not_a_value() {
        assert_eq!(
            resolve("Host a\n  ConnectTimeout none\n  ConnectTimeout 5\n", "a").connect_timeout,
            Some(5)
        );
        assert_eq!(
            resolve("Host a\n  ConnectTimeout 5\n  ConnectTimeout none\n", "a").connect_timeout,
            Some(5)
        );
        assert_eq!(
            resolve("Host a\n  ConnectTimeout none\n", "a").connect_timeout,
            None
        );
    }

    /// A value under a NON-matching `Host` block does not apply.
    /// Oracle: `Host other` + `ConnectTimeout 9` dumps `connecttimeout
    /// none` for alias `t`.
    #[test]
    fn connect_timeout_is_host_scoped() {
        let text = "Host other\n  ConnectTimeout 9\nHost t\n  Port 2222\n";
        let resolved = resolve(text, "t");
        assert_eq!(resolved.connect_timeout, None);
        assert_eq!(resolved.port, Some(2222));
    }

    /// The refusals, upstream's own wording (openssh/readconf.c:1218-1227):
    /// a missing or empty value says `missing time value.`, anything
    /// convtime rejects says `invalid time value.` - including `NONE`,
    /// because the `none` strcmp is case-SENSITIVE.
    #[test]
    fn connect_timeout_refusals_use_upstreams_wording() {
        for (text, reason) in [
            ("Host a\n  ConnectTimeout #x\n", "missing time value."),
            ("Host a\n  ConnectTimeout \"\"\n", "missing time value."),
            ("Host a\n  ConnectTimeout bogus\n", "invalid time value."),
            ("Host a\n  ConnectTimeout -5\n", "invalid time value."),
            ("Host a\n  ConnectTimeout NONE\n", "invalid time value."),
        ] {
            assert_eq!(
                refusal(text, "a"),
                format!("<ssh_config> line 2: {reason}"),
                "fixture {text:?}"
            );
        }
    }

    /// The exposure-surfaced defect this change fixes: upstream refuses a
    /// bad line from a NON-matching `Host` block too, because `*activep`
    /// gates only the assignment (openssh/readconf.c:1229) while the parse
    /// runs for every line. oc used to skip inactive blocks wholesale.
    /// Oracle: both fixtures exit 255 for alias `t`, refusing line 2.
    #[test]
    fn a_bad_value_in_a_non_matching_block_still_refuses() {
        assert_eq!(
            refusal("Host other\n  ConnectTimeout bogus\nHost t\n", "t"),
            "<ssh_config> line 2: invalid time value."
        );
        // The hoisted gate covers the OTHER value options' missing-arg
        // refusal too - the same upstream rule, same measurement.
        assert_eq!(
            refusal("Host other\n  Port #x\nHost t\n", "t"),
            "<ssh_config> line 2: Missing argument."
        );
    }

    /// A refusal from a real file names that file, not the inline
    /// placeholder - the path really is threaded through.
    #[test]
    fn a_refusal_names_the_file_it_came_from() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("ssh_config");
        std::fs::write(&path, "Host a \"\" b\n").expect("write fixture");
        let err = resolve_host(&path, "a").expect_err("refused");
        assert_eq!(
            err.to_string(),
            format!("{} line 1: keyword host empty argument", path.display())
        );
    }

    /// Renders the resolved identity files as strings. `PathBuf` equality
    /// compares components, so it collapses separator runs and treats `/` as a
    /// separator on Windows - both of which would hide exactly the defects the
    /// pins below exist to catch.
    fn identity_file_strings(resolved: &ResolvedHost) -> Vec<String> {
        resolved
            .identity_files
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect()
    }

    /// Before the fix the expansion went through `PathBuf::join`, whose
    /// absolute-path rule replaced the accumulated path outright and dropped
    /// the home directory, yielding `/probe`.
    /// upstream: openssh/misc.c:1281 collapses the run instead.
    #[test]
    fn tilde_slash_run_keeps_home_prefix() {
        let home = home_dir().expect("HOME or USERPROFILE must be set");
        let text = "Host example\n  IdentityFile ~//probe\n";
        let resolved = resolve(text, "example");
        let expected = format!(
            "{}{}probe",
            home.to_string_lossy(),
            std::path::MAIN_SEPARATOR
        );
        assert_eq!(identity_file_strings(&resolved), vec![expected]);
    }

    #[test]
    fn identity_file_tilde_expands_through_the_directive() {
        let home = home_dir().expect("HOME or USERPROFILE must be set");
        let text = "Host example\n  IdentityFile ~/.ssh/id_probe\n";
        let resolved = resolve(text, "example");
        let sep = std::path::MAIN_SEPARATOR;
        let expected = format!("{}{sep}.ssh{sep}id_probe", home.to_string_lossy());
        assert_eq!(identity_file_strings(&resolved), vec![expected]);
    }

    #[test]
    fn identity_agent_tilde_expands_through_the_directive() {
        let home = home_dir().expect("HOME or USERPROFILE must be set");
        let text = "Host example\n  IdentityAgent ~/agent.sock\n";
        let resolved = resolve(text, "example");
        let expected = format!(
            "{}{}agent.sock",
            home.to_string_lossy(),
            std::path::MAIN_SEPARATOR
        );
        assert_eq!(resolved.identity_agent.as_deref(), Some(expected.as_str()));
    }

    /// The reported defect: a `\`-separated home joined to a `/`-separated
    /// remainder. Pinned by value with `sep` supplied explicitly, because the
    /// Windows join character is not observable through `PathBuf` on a Unix
    /// host.
    #[test]
    fn windows_expansion_emits_no_forward_slash() {
        let out = expand_tilde_with("~/.ssh/id_probe", Some(r"C:\Users\x"), '\\');
        assert_eq!(out, r"C:\Users\x\.ssh\id_probe");
        assert!(!out.contains('/'), "mixed separators in {out}");
    }

    /// upstream: openssh/misc.c:1284 - the Win32 fork's `WINDOWS` arm accepts a
    /// backslash directly after the tilde.
    #[test]
    fn windows_accepts_backslash_after_tilde() {
        let out = expand_tilde_with(r"~\.ssh\id_probe", Some(r"C:\Users\x"), '\\');
        assert_eq!(out, r"C:\Users\x\.ssh\id_probe");
    }

    /// On Unix a backslash is an ordinary filename character and must survive
    /// expansion untouched, so the rewrite above is strictly a Windows arm.
    #[test]
    fn unix_expansion_preserves_literal_backslash() {
        let out = expand_tilde_with(r"~/od\d", Some("/home/u"), '/');
        assert_eq!(out, r"/home/u/od\d");
        assert_eq!(expand_tilde_with(r"~\od", Some("/home/u"), '/'), r"~\od");
    }

    /// upstream: openssh/misc.c:1281 `copy += strspn(copy, "/")`.
    #[test]
    fn separator_run_after_tilde_is_collapsed() {
        assert_eq!(
            expand_tilde_with("~//probe", Some("/home/u"), '/'),
            "/home/u/probe"
        );
        assert_eq!(
            expand_tilde_with(r"~\\probe", Some(r"C:\Users\x"), '\\'),
            r"C:\Users\x\probe"
        );
    }

    /// upstream: openssh/misc.c:1309 - the trailing separator on the home
    /// directory is not doubled.
    #[test]
    fn trailing_separator_on_home_is_not_doubled() {
        assert_eq!(
            expand_tilde_with("~/probe", Some("/home/u/"), '/'),
            "/home/u/probe"
        );
        assert_eq!(
            expand_tilde_with("~/probe", Some(r"C:\Users\x\"), '\\'),
            r"C:\Users\x\probe"
        );
    }

    #[test]
    fn unexpandable_tilde_forms_are_left_alone() {
        assert_eq!(expand_tilde_with("~", Some("/home/u"), '/'), "~");
        assert_eq!(
            expand_tilde_with("~other/k", Some("/home/u"), '/'),
            "~other/k"
        );
        assert_eq!(expand_tilde_with("~/k", None, '/'), "~/k");
        assert_eq!(expand_tilde_with("/abs/k", Some("/home/u"), '/'), "/abs/k");
    }

    /// The live wrapper must pick the platform's own separator; this is what
    /// carries the by-value pins above onto the real path.
    #[test]
    fn live_wrapper_uses_the_platform_separator() {
        let home = home_dir().expect("HOME or USERPROFILE must be set");
        let expanded = expand_tilde_str("~/a/b");
        let expected = format!(
            "{}{}a{}b",
            home.to_string_lossy(),
            std::path::MAIN_SEPARATOR,
            std::path::MAIN_SEPARATOR
        );
        assert_eq!(expanded, expected);
    }

    #[test]
    fn pattern_question_mark_matches_single_char() {
        assert!(pattern_matches("a", "?"));
        assert!(!pattern_matches("ab", "?"));
        assert!(pattern_matches("ab", "??"));
    }

    // -- file load order (task 237e) ----------------------------------
    //
    // upstream: openssh/ssh.c:561-592 `process_config_files()` reads the
    // user file, then the system file, into ONE options struct. The
    // system path is injected through the `ConfigFile` seam rather than
    // read from the host's real `/etc/ssh/ssh_config`.

    /// Materialises `text` as a config file and returns its
    /// [`ConfigFile`] row for a composed-resolve fixture.
    fn fixture_file(
        dir: &tempfile::TempDir,
        name: &str,
        text: &str,
        check_perm: bool,
    ) -> ConfigFile {
        let path = dir.path().join(name);
        std::fs::write(&path, text).expect("write fixture");
        // These load-order cells do not use `Include`, so `user_conf` is
        // immaterial; tie it to `check_perm` (both hold for the default user
        // file, neither for the injected system file).
        ConfigFile {
            path,
            check_perm,
            user_conf: check_perm,
        }
    }

    /// Cell (a): a scalar keyword set in BOTH files keeps the USER
    /// file's value - the claim state crosses the file boundary, so the
    /// system file cannot re-claim a taken slot
    /// (openssh/readconf.c:1229 over openssh/ssh.c:571-589).
    #[test]
    fn user_file_claims_scalar_slots_before_the_system_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let user = fixture_file(&dir, "user", "Host t\n  Port 1111\n  User first\n", false);
        let system = fixture_file(
            &dir,
            "system",
            "Host *\n  Port 2222\n  User second\n  HostName sys.example.com\n",
            false,
        );
        let resolved = resolve_host_files(&[user, system], "t").expect("accepted");
        assert_eq!(resolved.port, Some(1111));
        assert_eq!(resolved.user.as_deref(), Some("first"));
        // A slot the user file left unclaimed IS the system file's to
        // claim - cell (b) for this reader, red before this change.
        assert_eq!(resolved.hostname.as_deref(), Some("sys.example.com"));
    }

    /// `IdentityFile` keeps its `Accumulate` policy across the boundary:
    /// both files' identities land, in load order
    /// (openssh/readconf.c:1394 `add_identity_file` has no unset test).
    #[test]
    fn identity_files_accumulate_across_both_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let user = fixture_file(&dir, "user", "Host t\n  IdentityFile /user/key\n", false);
        let system = fixture_file(
            &dir,
            "system",
            "Host *\n  IdentityFile /system/key\n",
            false,
        );
        let resolved = resolve_host_files(&[user, system], "t").expect("accepted");
        assert_eq!(
            resolved.identity_files,
            vec![PathBuf::from("/user/key"), PathBuf::from("/system/key")]
        );
    }

    /// A missing user file is skipped and the system file still applies
    /// (openssh/ssh.c:580-589 discards the default reads' results).
    #[test]
    fn a_missing_user_file_still_reaches_the_system_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let user = ConfigFile {
            path: dir.path().join("nonexistent"),
            check_perm: true,
            user_conf: true,
        };
        let system = fixture_file(&dir, "system", "Host *\n  Port 2222\n", false);
        let resolved = resolve_host_files(&[user, system], "t").expect("accepted");
        assert_eq!(resolved.port, Some(2222));
    }

    /// A refusal in the user file aborts the whole load: the error names
    /// the USER file even though the system file carries the same defect
    /// on a different line, proving the system file was never scanned
    /// (openssh/readconf.c:2667 fatals before the second read).
    #[test]
    fn a_refused_user_file_stops_before_the_system_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let user = fixture_file(&dir, "user", "Host \"broken\n", false);
        let system = fixture_file(&dir, "system", "ok line\nHost \"broken\n", false);
        let err = resolve_host_files(&[user.clone(), system], "t").expect_err("refused");
        assert_eq!(
            err.to_string(),
            format!("{} line 1: invalid quotes", user.path.display())
        );
    }

    /// Cell (d), embedded half: the world-writable DEFAULT user file is
    /// refused with upstream's fatal wording, while the SAME mode on an
    /// explicit (`-F`-shaped, `check_perm: false`) file is accepted -
    /// CHECKPERM's scope is the flag, not the file
    /// (openssh/ssh.c:571-583, openssh/readconf.c:2579-2587).
    #[cfg(unix)]
    #[test]
    fn checkperm_refuses_the_default_user_file_but_not_an_explicit_one() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let mut file = fixture_file(&dir, "config", "Host t\n  Port 2222\n", true);
        std::fs::set_permissions(&file.path, std::fs::Permissions::from_mode(0o666))
            .expect("chmod");
        let err = resolve_host_files(&[file.clone()], "t").expect_err("refused");
        assert_eq!(
            err.to_string(),
            format!("Bad owner or permissions on {}", file.path.display())
        );
        file.check_perm = false;
        let resolved = resolve_host_files(&[file], "t").expect("explicit file accepted");
        assert_eq!(resolved.port, Some(2222));
    }

    // -- Include (task 237f) ------------------------------------------
    //
    // upstream: the `oInclude` arm, openssh/readconf.c:2073-2150. The
    // anchors (`~/.ssh` vs `/etc/ssh`, `~`) are injected through
    // `IncludeAnchors` so the cells never touch the host's real config
    // directories.

    /// Writes `text` at `path`, creating parent directories, and returns
    /// `path`.
    fn write_at(path: PathBuf, text: &str) -> PathBuf {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir -p");
        }
        std::fs::write(&path, text).expect("write include fixture");
        path
    }

    /// An explicit-file (`-F`-shaped) config row holding `text`, so the
    /// perm check never interferes with an Include cell.
    fn include_top(dir: &tempfile::TempDir, text: &str, user_conf: bool) -> ConfigFile {
        ConfigFile {
            path: write_at(dir.path().join("top"), text),
            check_perm: false,
            user_conf,
        }
    }

    /// Anchors rooted under `dir`: `home`, `home/.ssh` (the userconf
    /// anchor) and `etc/ssh` (the system anchor).
    fn anchors_under(dir: &tempfile::TempDir) -> IncludeAnchors {
        let home = dir.path().join("home");
        IncludeAnchors {
            user_dir: Some(home.join(".ssh")),
            home: Some(home),
            system_dir: dir.path().join("etc").join("ssh"),
        }
    }

    /// An absolute-path Include pulls a scalar out of another file, under
    /// the current (active) block state.
    #[test]
    fn absolute_include_pulls_in_a_scalar() {
        let dir = tempfile::tempdir().expect("tempdir");
        let snippet = write_at(
            dir.path().join("snippet"),
            "Host t\n  HostName inc.example\n",
        );
        let top = include_top(&dir, &format!("Include {}\n", snippet.display()), true);
        let resolved = resolve_host_files_with_anchors(&[top], "t", &IncludeAnchors::none())
            .expect("accepted");
        assert_eq!(resolved.hostname.as_deref(), Some("inc.example"));
    }

    /// A glob expands in sorted order, so with first-obtained-wins the
    /// alphabetically-first match claims the slot. The control (only the
    /// second file present) proves the ORDER decides the winner, not merely
    /// which files exist.
    #[test]
    fn glob_include_reads_matches_in_sorted_order() {
        let dir = tempfile::tempdir().expect("tempdir");
        let inc = dir.path().join("inc");
        write_at(inc.join("01.conf"), "Host *\n  User first\n");
        write_at(inc.join("02.conf"), "Host *\n  User second\n");
        let top = include_top(&dir, &format!("Include {}/*.conf\n", inc.display()), true);
        let resolved =
            resolve_host_files_with_anchors(&[top.clone()], "t", &IncludeAnchors::none())
                .expect("accepted");
        assert_eq!(resolved.user.as_deref(), Some("first"));

        // Control: drop the sorted-first file and the second one now wins,
        // so the winner tracks glob order rather than a fixed preference.
        std::fs::remove_file(inc.join("01.conf")).expect("rm");
        let resolved = resolve_host_files_with_anchors(&[top], "t", &IncludeAnchors::none())
            .expect("accepted");
        assert_eq!(resolved.user.as_deref(), Some("second"));
    }

    /// A relative Include in a USER config anchors under `~/.ssh`; in a
    /// SYSTEM config the SAME spelling anchors under `/etc/ssh`. The two
    /// halves place different files at the two anchors and prove each config
    /// kind reads its own.
    #[test]
    fn relative_include_anchors_by_config_kind() {
        let dir = tempfile::tempdir().expect("tempdir");
        let anchors = anchors_under(&dir);
        write_at(
            anchors.user_dir.clone().unwrap().join("inc.conf"),
            "Host t\n  User fromuser\n",
        );
        write_at(
            anchors.system_dir.join("inc.conf"),
            "Host t\n  User fromsystem\n",
        );

        let user_top = include_top(&dir, "Include inc.conf\n", true);
        assert_eq!(
            resolve_host_files_with_anchors(&[user_top], "t", &anchors)
                .expect("accepted")
                .user
                .as_deref(),
            Some("fromuser")
        );

        let system_top = ConfigFile {
            path: write_at(dir.path().join("systop"), "Include inc.conf\n"),
            check_perm: false,
            user_conf: false,
        };
        assert_eq!(
            resolve_host_files_with_anchors(&[system_top], "t", &anchors)
                .expect("accepted")
                .user
                .as_deref(),
            Some("fromsystem")
        );
    }

    /// A `~/`-prefixed Include expands against the home directory in a user
    /// config, but is REFUSED in a system config with upstream's wording
    /// (openssh/readconf.c:2095-2098).
    #[test]
    fn tilde_include_is_user_only() {
        let dir = tempfile::tempdir().expect("tempdir");
        let anchors = anchors_under(&dir);
        write_at(
            anchors.home.clone().unwrap().join("inc.conf"),
            "Host t\n  Port 2020\n",
        );

        let user_top = include_top(&dir, "Include ~/inc.conf\n", true);
        assert_eq!(
            resolve_host_files_with_anchors(&[user_top], "t", &anchors)
                .expect("accepted")
                .port,
            Some(2020)
        );

        let system_top = ConfigFile {
            path: write_at(dir.path().join("systop"), "Include ~/inc.conf\n"),
            check_perm: false,
            user_conf: false,
        };
        let err =
            resolve_host_files_with_anchors(&[system_top], "t", &anchors).expect_err("refused");
        assert!(
            err.to_string().ends_with("bad include path ~/inc.conf."),
            "unexpected: {err}"
        );
    }

    /// A glob that matches nothing is tolerated (upstream's `GLOB_NOMATCH`
    /// arm, openssh/readconf.c:2122-2126): the load succeeds and the missing
    /// include contributes nothing. The control include that DOES match
    /// proves the resolve was otherwise live.
    #[test]
    fn missing_glob_is_tolerated() {
        let dir = tempfile::tempdir().expect("tempdir");
        let present = write_at(dir.path().join("present"), "Host t\n  Port 7\n");
        let top = include_top(
            &dir,
            &format!(
                "Include {}/does-not-exist-*.conf\nInclude {}\n",
                dir.path().display(),
                present.display()
            ),
            true,
        );
        let resolved = resolve_host_files_with_anchors(&[top], "t", &IncludeAnchors::none())
            .expect("accepted");
        assert_eq!(resolved.port, Some(7));
    }

    /// A present include file that fails the owner/permission check is
    /// fatal, not skipped like a missing one - upstream adds
    /// `SSHCONF_CHECKPERM` to the recursion (openssh/readconf.c:2129).
    #[cfg(unix)]
    #[test]
    fn present_include_failing_the_perm_check_is_fatal() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let snippet = write_at(dir.path().join("snippet"), "Host t\n  Port 9\n");
        std::fs::set_permissions(&snippet, std::fs::Permissions::from_mode(0o666)).expect("chmod");
        let top = include_top(&dir, &format!("Include {}\n", snippet.display()), true);
        let err = resolve_host_files_with_anchors(&[top], "t", &IncludeAnchors::none())
            .expect_err("refused");
        assert_eq!(
            err.to_string(),
            format!("Bad owner or permissions on {}", snippet.display())
        );
    }

    /// A self-including file terminates at the depth ceiling rather than
    /// recursing forever (READCONF_MAX_DEPTH, openssh/readconf.c:2573-2574).
    #[test]
    fn include_depth_limit_is_fatal() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("self");
        write_at(path.clone(), &format!("Include {}\n", path.display()));
        let top = ConfigFile {
            path,
            check_perm: false,
            user_conf: true,
        };
        let err = resolve_host_files_with_anchors(&[top], "t", &IncludeAnchors::none())
            .expect_err("refused");
        assert!(
            err.to_string()
                .ends_with("Too many recursive configuration includes"),
            "unexpected: {err}"
        );
    }

    /// An empty Include argument (a quoted empty token) is refused with
    /// upstream's wording (openssh/readconf.c:2081-2083).
    #[test]
    fn empty_include_argument_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let top = include_top(&dir, "Include \"\"\n", true);
        let err = resolve_host_files_with_anchors(&[top], "t", &IncludeAnchors::none())
            .expect_err("refused");
        assert!(
            err.to_string().ends_with("keyword include empty argument"),
            "unexpected: {err}"
        );
    }

    /// An Include inside a matching Host block reads its file under the
    /// active state, so the file's own top-level directive applies. Under a
    /// NON-matching block the include still runs but recurses NEVERMATCH, so
    /// nothing from it lands - the active-state save/restore across the
    /// include boundary (openssh/readconf.c:2130, :2144).
    #[test]
    fn include_inherits_the_block_active_state() {
        let dir = tempfile::tempdir().expect("tempdir");
        let snippet = write_at(dir.path().join("snippet"), "User included\n");

        // Matching block: the include's top-level directive applies.
        let matching = include_top(
            &dir,
            &format!("Host t\n  Include {}\n", snippet.display()),
            true,
        );
        assert_eq!(
            resolve_host_files_with_anchors(&[matching], "t", &IncludeAnchors::none())
                .expect("accepted")
                .user
                .as_deref(),
            Some("included")
        );

        // Non-matching block: the include is walked (a refusal would still
        // fire) but assigns nothing under NEVERMATCH.
        let non_matching = ConfigFile {
            path: write_at(
                dir.path().join("top2"),
                &format!("Host other\n  Include {}\n", snippet.display()),
            ),
            check_perm: false,
            user_conf: true,
        };
        assert!(
            resolve_host_files_with_anchors(&[non_matching], "t", &IncludeAnchors::none())
                .expect("accepted")
                .user
                .is_none()
        );
    }

    /// A `Host` block opened inside an included file only activates when the
    /// include ran from an active context: NEVERMATCH propagates into the
    /// file so its own `Host *` cannot match, while from an active context
    /// the same block applies.
    #[test]
    fn included_host_block_obeys_never_match() {
        let dir = tempfile::tempdir().expect("tempdir");
        let snippet = write_at(dir.path().join("snippet"), "Host *\n  Port 4242\n");

        let active = include_top(
            &dir,
            &format!("Host t\n  Include {}\n", snippet.display()),
            true,
        );
        assert_eq!(
            resolve_host_files_with_anchors(&[active], "t", &IncludeAnchors::none())
                .expect("accepted")
                .port,
            Some(4242)
        );

        let inactive = ConfigFile {
            path: write_at(
                dir.path().join("top2"),
                &format!("Host other\n  Include {}\n", snippet.display()),
            ),
            check_perm: false,
            user_conf: true,
        };
        assert!(
            resolve_host_files_with_anchors(&[inactive], "t", &IncludeAnchors::none())
                .expect("accepted")
                .port
                .is_none()
        );
    }

    /// The block state is RESTORED after an include: a top-level include
    /// that opens a non-matching `Host` block does not leak that block's
    /// inactive state back to the including file, whose later directives
    /// still apply. This is upstream's `*activep = oactive` restore
    /// (openssh/readconf.c:2144).
    #[test]
    fn block_state_restores_after_the_include() {
        let dir = tempfile::tempdir().expect("tempdir");
        // The included file ends inside a NON-matching Host block.
        let snippet = write_at(dir.path().join("snippet"), "Host nomatch\n  User dead\n");
        let top = include_top(
            &dir,
            &format!("Include {}\nUser afterwards\n", snippet.display()),
            true,
        );
        // `User afterwards` sits at the including file's top level; if the
        // include leaked its trailing inactive block, this would be dropped.
        assert_eq!(
            resolve_host_files_with_anchors(&[top], "t", &IncludeAnchors::none())
                .expect("accepted")
                .user
                .as_deref(),
            Some("afterwards")
        );
    }
}

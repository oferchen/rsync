//! ssh_config percent-token and `${ENV}` expansion, mirroring OpenSSH's
//! client-side expanders (openssh/misc.c:1263-1428 `vdollar_percent_expand`,
//! `percent_expand`, `percent_dollar_expand`).
//!
//! This module is the ONE owner of the expansion mechanics. WHICH tokens an
//! option receives is a per-option decision made at the option's use site,
//! never a uniform rule - upstream expands each option with an explicit
//! key list at the point the value is consumed:
//!
//! * `Hostname` - `%%`/`%h` only, against the command-line alias
//!   (openssh/ssh.c:1216-1222).
//! * `IdentityFile`/`CertificateFile` - tilde, then the default client
//!   token set plus `${ENV}` (openssh/ssh.c:2369-2419).
//! * `IdentityAgent`, `RevokedHostKeys`, `UserKnownHostsFile` - tilde,
//!   then the default set plus `${ENV}` (openssh/ssh.c:1459-1526).
//! * `GlobalKnownHostsFile` - tilde ONLY, no percent and no env
//!   (openssh/ssh.c:1751 `tilde_expand_paths`).
//! * `ProxyCommand`/`ProxyJump` - `%h %k %n %p %r` only, at dial time
//!   (openssh/sshconnect.c:89-107 `expand_proxy_command`).
//!
//! The default client set is the 12 tokens of
//! `DEFAULT_CLIENT_PERCENT_EXPAND_ARGS` (openssh/sshconnect.h:56-69):
//! `%C %L %i %k %l %n %p %d %h %r %u %j`.

use super::error::SshError;
use sha1::{Digest, Sha1};

/// Expands `%`-tokens in `template` against the `(letter, replacement)`
/// pairs in `keys`. `%%` is a literal `%`; an unknown token or a trailing
/// lone `%` is an error, exactly as upstream's expander is fatal there
/// (openssh/misc.c:1345-1363).
///
/// # Errors
///
/// [`SshError::TokenExpansion`] carrying upstream's reason text
/// (`unknown key %X` / `invalid format`).
pub(super) fn percent_expand(
    template: &str,
    keys: &[(char, &str)],
    option: &str,
) -> Result<String, SshError> {
    expand(template, keys, false, option)
}

/// [`percent_expand`] plus `${NAME}` environment expansion
/// (openssh/misc.c:1304-1330). An unset variable, an unterminated `${`,
/// or an empty `${}` is an error - upstream's caller is fatal on all
/// three (openssh/ssh.c:240-241 `invalid environment variable expansion`).
///
/// # Errors
///
/// [`SshError::TokenExpansion`], as for [`percent_expand`].
pub(super) fn percent_dollar_expand(
    template: &str,
    keys: &[(char, &str)],
    option: &str,
) -> Result<String, SshError> {
    expand(template, keys, true, option)
}

/// upstream: openssh/misc.c:1263 `vdollar_percent_expand` - one scanner
/// with the `${}` arm switched per caller.
fn expand(
    template: &str,
    keys: &[(char, &str)],
    dollar: bool,
    option: &str,
) -> Result<String, SshError> {
    let refuse = |reason: String| SshError::TokenExpansion {
        option: option.to_owned(),
        reason,
    };
    let mut out = String::with_capacity(template.len());
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        if dollar && c == '$' && chars.peek() == Some(&'{') {
            chars.next();
            let mut name = String::new();
            loop {
                match chars.next() {
                    Some('}') => break,
                    Some(v) => name.push(v),
                    None => {
                        return Err(refuse(format!(
                            "environment variable '{name}' missing closing '}}'"
                        )));
                    }
                }
            }
            if name.is_empty() {
                return Err(refuse("zero-length environment variable".to_owned()));
            }
            match std::env::var(&name) {
                Ok(value) => out.push_str(&value),
                // upstream reports the missing variable and the caller
                // aborts (openssh/misc.c:1318-1321, openssh/ssh.c:240-241).
                Err(_) => return Err(refuse(format!("env var ${{{name}}} has no value"))),
            }
            continue;
        }
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('%') => out.push('%'),
            Some(token) => match keys.iter().find(|(k, _)| *k == token) {
                Some((_, repl)) => out.push_str(repl),
                None => return Err(refuse(format!("unknown key %{token}"))),
            },
            None => return Err(refuse("invalid format".to_owned())),
        }
    }
    Ok(out)
}

/// The connection strings the default client token set expands from.
/// upstream: openssh/ssh.c:1419-1440 builds the same struct once the
/// hostname, port and user are FINAL (post config resolution), and
/// openssh/sshconnect.h:36-53 declares the fields.
pub(super) struct ConnInfo {
    conn_hash_hex: String,
    shorthost: String,
    uidstr: String,
    keyalias: String,
    thishost: String,
    host_arg: String,
    portstr: String,
    homedir: String,
    remhost: String,
    remuser: String,
    locuser: String,
    jmphost: String,
}

impl ConnInfo {
    /// Builds the token values from the resolved connection parameters.
    ///
    /// `remhost` is the host AFTER any `Hostname` rewrite; `host_arg` is
    /// the alias the user typed (`%n`). `%k` falls back to the alias when
    /// no `HostKeyAlias` is set (openssh/ssh.c:1428-1430). `%r` falls back
    /// to the local login name, upstream's `options.user` default
    /// (openssh/ssh.c:1096-1097).
    pub(super) fn new(
        remhost: &str,
        host_arg: &str,
        port: u16,
        remuser: Option<&str>,
        host_key_alias: Option<&str>,
        jump_hosts: Option<&str>,
    ) -> Self {
        let thishost = local_hostname();
        let shorthost = thishost.split('.').next().unwrap_or_default().to_owned();
        let portstr = port.to_string();
        let remuser = remuser.map(str::to_owned).unwrap_or_else(local_user_name);
        let jmphost = jump_last_host(jump_hosts);
        let conn_hash_hex = connection_hash(&thishost, remhost, &portstr, &remuser, &jmphost);
        Self {
            conn_hash_hex,
            shorthost,
            uidstr: local_uid_string(),
            keyalias: host_key_alias.unwrap_or(host_arg).to_owned(),
            thishost,
            host_arg: host_arg.to_owned(),
            portstr,
            homedir: home_dir_string(),
            remhost: remhost.to_owned(),
            remuser,
            locuser: local_user_name(),
            jmphost,
        }
    }

    /// The 12 default client tokens, in upstream's declaration order.
    /// upstream: openssh/sshconnect.h:56-69
    /// `DEFAULT_CLIENT_PERCENT_EXPAND_ARGS`.
    pub(super) fn default_keys(&self) -> [(char, &str); 12] {
        [
            ('C', &self.conn_hash_hex),
            ('L', &self.shorthost),
            ('i', &self.uidstr),
            ('k', &self.keyalias),
            ('l', &self.thishost),
            ('n', &self.host_arg),
            ('p', &self.portstr),
            ('d', &self.homedir),
            ('h', &self.remhost),
            ('r', &self.remuser),
            ('u', &self.locuser),
            ('j', &self.jmphost),
        ]
    }
}

/// Expands with the default client token set, no environment expansion.
/// upstream: openssh/ssh.c:218-225 `default_client_percent_expand`.
#[cfg(test)]
pub(super) fn default_client_percent_expand(
    template: &str,
    info: &ConnInfo,
    option: &str,
) -> Result<String, SshError> {
    percent_expand(template, &info.default_keys(), option)
}

/// Expands with the default client token set AND `${ENV}` substitution.
/// upstream: openssh/ssh.c:232-244 `default_client_percent_dollar_expand`.
pub(super) fn default_client_percent_dollar_expand(
    template: &str,
    info: &ConnInfo,
    option: &str,
) -> Result<String, SshError> {
    percent_dollar_expand(template, &info.default_keys(), option)
}

/// `%C`: lowercase SHA1 hex over the concatenated connection strings.
/// upstream: openssh/readconf.c:353-370 `ssh_connection_hash` (SHA1 via
/// `tohex`, which renders lowercase).
fn connection_hash(
    thishost: &str,
    host: &str,
    portstr: &str,
    user: &str,
    jumphost: &str,
) -> String {
    let mut hasher = Sha1::new();
    hasher.update(thishost.as_bytes());
    hasher.update(host.as_bytes());
    hasher.update(portstr.as_bytes());
    hasher.update(user.as_bytes());
    hasher.update(jumphost.as_bytes());
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// `%j`: the host portion of the ProxyJump chain's LAST hop, or empty when
/// no jump is configured or it is the literal `none`. upstream:
/// openssh/readconf.c:2318-2341 `parse_jump` keeps the last comma element
/// as `jump_host`, and openssh/ssh.c:1436-1437 maps absent/`none` to `""`.
fn jump_last_host(jump: Option<&str>) -> String {
    let Some(jump) = jump else {
        return String::new();
    };
    let jump = jump.trim();
    if jump.eq_ignore_ascii_case("none") {
        return String::new();
    }
    let last = jump.rsplit(',').next().unwrap_or(jump).trim();
    super::proxy::parse_jump_hop(last).host
}

/// `%u` (and the `%r` fallback): the local login name from the environment,
/// the transport's portable stand-in for upstream's `pw->pw_name`
/// (openssh/ssh.c:1434).
pub(super) fn local_user_name() -> String {
    #[cfg(windows)]
    let var = "USERNAME";
    #[cfg(not(windows))]
    let var = "USER";
    std::env::var(var).unwrap_or_default()
}

/// `%l`/`%L`: the local host name. upstream: openssh/ssh.c:1421
/// `gethostname()`; the syscall lives in `platform` per the unsafe-code
/// policy.
fn local_hostname() -> String {
    #[cfg(unix)]
    {
        platform::env::hostname().unwrap_or_default()
    }
    #[cfg(windows)]
    {
        std::env::var("COMPUTERNAME").unwrap_or_default()
    }
}

/// `%i`: the real uid. upstream: openssh/ssh.c:1423-1424. Windows has no
/// uid; `0` keeps the token expandable rather than fatal there.
fn local_uid_string() -> String {
    #[cfg(unix)]
    {
        platform::privilege::real_uid().to_string()
    }
    #[cfg(windows)]
    {
        "0".to_owned()
    }
}

/// `%d`: the caller's home directory, upstream's `pw->pw_dir`
/// (openssh/ssh.c:1433). Same environment source as the tilde expansion in
/// the config reader.
fn home_dir_string() -> String {
    #[cfg(windows)]
    let var = "USERPROFILE";
    #[cfg(not(windows))]
    let var = "HOME";
    std::env::var(var).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use platform::env::EnvGuard;
    use std::ffi::OsStr;
    use std::sync::Mutex;

    // Serialises the tests that mutate the process environment.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn info() -> ConnInfo {
        ConnInfo::new(
            "final.example",
            "alias",
            2222,
            Some("deploy"),
            None,
            Some("jumper.example:44,u@last.example:2200"),
        )
    }

    #[test]
    fn double_percent_is_a_literal_percent() {
        let out = percent_expand("100%% of %h", &[('h', "x")], "Test").expect("expands");
        assert_eq!(out, "100% of x");
    }

    #[test]
    fn unknown_token_is_refused_with_upstream_wording() {
        // upstream: openssh/misc.c:1358-1361 `unknown key %%%c` is fatal.
        let err = percent_expand("%q", &[('h', "x")], "Test").unwrap_err();
        assert!(err.to_string().contains("unknown key %q"), "{err}");
    }

    #[test]
    fn trailing_lone_percent_is_refused() {
        // upstream: openssh/misc.c:1348-1351 `invalid format` is fatal.
        let err = percent_expand("oops %", &[('h', "x")], "Test").unwrap_err();
        assert!(err.to_string().contains("invalid format"), "{err}");
    }

    #[test]
    fn env_expansion_substitutes_a_set_variable() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _guard = EnvGuard::set("OC_T1210_SET", OsStr::new("payload"));
        let out =
            percent_dollar_expand("pre-${OC_T1210_SET}-post", &[('h', "x")], "Test").expect("ok");
        assert_eq!(out, "pre-payload-post");
    }

    #[test]
    fn env_expansion_refuses_an_unset_variable() {
        // upstream: openssh/misc.c:1318-1321 records the miss and
        // openssh/ssh.c:240-241 aborts on it.
        let _lock = ENV_LOCK.lock().unwrap();
        let _guard = EnvGuard::remove("OC_T1210_UNSET");
        let err = percent_dollar_expand("${OC_T1210_UNSET}", &[('h', "x")], "Test").unwrap_err();
        assert!(err.to_string().contains("has no value"), "{err}");
    }

    #[test]
    fn env_expansion_refuses_unterminated_and_empty_names() {
        // upstream: openssh/misc.c:1306-1316.
        let err = percent_dollar_expand("${OPEN", &[('h', "x")], "Test").unwrap_err();
        assert!(err.to_string().contains("missing closing"), "{err}");
        let err = percent_dollar_expand("${}", &[('h', "x")], "Test").unwrap_err();
        assert!(
            err.to_string().contains("zero-length environment variable"),
            "{err}"
        );
    }

    #[test]
    fn percent_expand_leaves_dollar_syntax_alone() {
        // The env arm only exists in the `percent_dollar` variant
        // (openssh/misc.c:1398-1409 passes dollar=0), so `${X}` is literal
        // text for options like `Hostname` and `ProxyCommand`.
        let out = percent_expand("${NOT_EXPANDED}", &[('h', "x")], "Test").expect("literal");
        assert_eq!(out, "${NOT_EXPANDED}");
    }

    #[test]
    fn default_set_expands_the_connection_tokens() {
        let info = info();
        let out =
            default_client_percent_expand("%h|%n|%p|%r|%k|%j", &info, "Test").expect("expands");
        assert_eq!(out, "final.example|alias|2222|deploy|alias|last.example");
    }

    #[test]
    fn conn_hash_is_forty_lowercase_hex_digits() {
        let info = info();
        let hash = default_client_percent_expand("%C", &info, "Test").expect("expands");
        assert_eq!(hash.len(), 40);
        assert!(
            hash.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
        // The hash is a function of the connection parameters: a different
        // port must change it (openssh/readconf.c:353-370 hashes portstr).
        let other = ConnInfo::new("final.example", "alias", 2223, Some("deploy"), None, None);
        let other_hash = default_client_percent_expand("%C", &other, "Test").expect("expands");
        assert_ne!(hash, other_hash);
    }

    #[test]
    fn keyalias_prefers_host_key_alias_over_the_alias() {
        // upstream: openssh/ssh.c:1428-1430.
        let info = ConnInfo::new("h", "alias", 22, Some("u"), Some("kalias"), None);
        let out = default_client_percent_expand("%k", &info, "Test").expect("expands");
        assert_eq!(out, "kalias");
    }

    #[test]
    fn jump_host_takes_the_last_hop_and_none_is_empty() {
        // upstream: openssh/readconf.c:2318-2341 (last comma element) and
        // openssh/ssh.c:1436-1437 (`none`/absent -> "").
        assert_eq!(jump_last_host(Some("a,b@c:22,[::1]:2000")), "::1");
        assert_eq!(jump_last_host(Some("none")), "");
        assert_eq!(jump_last_host(None), "");
    }
}

//! The `Ciphers`, `MACs`, `KexAlgorithms` and `HostKeyAlgorithms` lists.
//!
//! Three steps, each upstream's:
//!
//! 1. **Validation** when the line is read. A list that does not start with
//!    `-` must name only algorithms OpenSSH knows (openssh/readconf.c:1593-1664
//!    calling `ciphers_valid`, `mac_valid`, `kex_names_valid` and
//!    `sshkey_names_valid2`). Only `HostKeyAlgorithms` accepts wildcards.
//! 2. **Assembly** at connect time. A bare list replaces the default, `+`
//!    appends to it, `-` removes from it and `^` moves names to its head,
//!    with patterns expanded against every known algorithm
//!    (openssh/kex-names.c:243-329 `kex_assemble_names`).
//! 3. **Mapping** onto what the embedded transport can negotiate. A name
//!    OpenSSH knows and russh does not is left out with a warning; a list
//!    with nothing left is refused, as an empty upstream list is
//!    (openssh/kex-names.c:318-323).
//!
//! The name tables are OpenSSH 10.0p1's as `ssh -Q` prints them for an
//! OpenSSL build: `cipher` (openssh/cipher.c:86-111), `mac`
//! (openssh/mac.c:59-79), `kex` (openssh/kex-names.c:54-90) and
//! `HostKeyAlgorithms` (openssh/sshkey.c `keyimpls`).

use super::error::SshError;
use crate::ssh::config_options::{PatternListMatch, match_pattern_list};
use logging::warn_log;

/// Which algorithm list a directive configures.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(in crate::ssh) enum AlgorithmList {
    /// `Ciphers` (openssh/readconf.c:254).
    Ciphers,
    /// `MACs` (openssh/readconf.c:255).
    Macs,
    /// `KexAlgorithms` (openssh/readconf.c:305).
    Kex,
    /// `HostKeyAlgorithms` (openssh/readconf.c:279).
    HostKey,
}

/// `ssh -Q cipher` (openssh/cipher.c:86-111; `none` is internal and never
/// listed, openssh/cipher.c:124-125).
const CIPHERS: &[&str] = &[
    "3des-cbc",
    "aes128-cbc",
    "aes192-cbc",
    "aes256-cbc",
    "aes128-ctr",
    "aes192-ctr",
    "aes256-ctr",
    "aes128-gcm@openssh.com",
    "aes256-gcm@openssh.com",
    "chacha20-poly1305@openssh.com",
];

/// `ssh -Q mac` (openssh/mac.c:59-79).
const MACS: &[&str] = &[
    "hmac-sha1",
    "hmac-sha1-96",
    "hmac-sha2-256",
    "hmac-sha2-512",
    "hmac-md5",
    "hmac-md5-96",
    "umac-64@openssh.com",
    "umac-128@openssh.com",
    "hmac-sha1-etm@openssh.com",
    "hmac-sha1-96-etm@openssh.com",
    "hmac-sha2-256-etm@openssh.com",
    "hmac-sha2-512-etm@openssh.com",
    "hmac-md5-etm@openssh.com",
    "hmac-md5-96-etm@openssh.com",
    "umac-64-etm@openssh.com",
    "umac-128-etm@openssh.com",
];

/// `ssh -Q kex` (openssh/kex-names.c:54-90).
const KEX: &[&str] = &[
    "diffie-hellman-group1-sha1",
    "diffie-hellman-group14-sha1",
    "diffie-hellman-group14-sha256",
    "diffie-hellman-group16-sha512",
    "diffie-hellman-group18-sha512",
    "diffie-hellman-group-exchange-sha1",
    "diffie-hellman-group-exchange-sha256",
    "ecdh-sha2-nistp256",
    "ecdh-sha2-nistp384",
    "ecdh-sha2-nistp521",
    "curve25519-sha256",
    "curve25519-sha256@libssh.org",
    "sntrup761x25519-sha512",
    "sntrup761x25519-sha512@openssh.com",
    "mlkem768x25519-sha256",
];

/// `ssh -Q HostKeyAlgorithms` (openssh/sshkey.c `keyimpls`, via
/// `sshkey_alg_list(0, 0, 1, ',')`, openssh/sshconnect2.c:244).
const HOST_KEYS: &[&str] = &[
    "ssh-ed25519",
    "ssh-ed25519-cert-v01@openssh.com",
    "sk-ssh-ed25519@openssh.com",
    "sk-ssh-ed25519-cert-v01@openssh.com",
    "ecdsa-sha2-nistp256",
    "ecdsa-sha2-nistp256-cert-v01@openssh.com",
    "ecdsa-sha2-nistp384",
    "ecdsa-sha2-nistp384-cert-v01@openssh.com",
    "ecdsa-sha2-nistp521",
    "ecdsa-sha2-nistp521-cert-v01@openssh.com",
    "sk-ecdsa-sha2-nistp256@openssh.com",
    "sk-ecdsa-sha2-nistp256-cert-v01@openssh.com",
    "webauthn-sk-ecdsa-sha2-nistp256@openssh.com",
    "ssh-rsa",
    "ssh-rsa-cert-v01@openssh.com",
    "rsa-sha2-256",
    "rsa-sha2-256-cert-v01@openssh.com",
    "rsa-sha2-512",
    "rsa-sha2-512-cert-v01@openssh.com",
];

impl AlgorithmList {
    /// Every name OpenSSH 10.0p1 accepts for this list.
    pub(in crate::ssh) fn known_names(self) -> &'static [&'static str] {
        match self {
            Self::Ciphers => CIPHERS,
            Self::Macs => MACS,
            Self::Kex => KEX,
            Self::HostKey => HOST_KEYS,
        }
    }

    /// The directive as the manual spells it.
    fn directive(self) -> &'static str {
        match self {
            Self::Ciphers => "Ciphers",
            Self::Macs => "MACs",
            Self::Kex => "KexAlgorithms",
            Self::HostKey => "HostKeyAlgorithms",
        }
    }

    /// The name upstream reports a failed composition under: the option
    /// field for the `ASSEMBLE` lists (openssh/readconf.c:2986-2997), the
    /// call site's text for host keys (openssh/sshconnect2.c:247).
    fn assembly_name(self) -> &'static str {
        match self {
            Self::Ciphers => "ciphers",
            Self::Macs => "macs",
            Self::Kex => "kex_algorithms",
            Self::HostKey => "expand HostKeyAlgorithms",
        }
    }

    /// Upstream's refusal for a list that fails validation
    /// (openssh/readconf.c:1602, :1619, :1637, :1658).
    pub(in crate::ssh) fn bad_spec(self, spec: &str) -> String {
        match self {
            Self::Ciphers => format!("Bad SSH2 cipher spec '{spec}'."),
            Self::Macs => format!("Bad SSH2 MAC spec '{spec}'."),
            Self::Kex => format!("Bad SSH2 KexAlgorithms '{spec}'."),
            Self::HostKey => format!("Bad key types '{spec}'."),
        }
    }

    /// Whether a directive's value passes upstream's parse-time check.
    ///
    /// A `-` list is never checked; a `+` or `^` prefix is stripped first
    /// (openssh/readconf.c:1600-1601). The validators stop at the first
    /// empty element, so `a,,junk` checks only `a`
    /// (openssh/cipher.c:224-225 `p && *p != '\0'`, and its siblings).
    pub(in crate::ssh) fn spec_is_valid(self, spec: &str) -> bool {
        if spec.starts_with('-') {
            return true;
        }
        let names = spec.strip_prefix(['+', '^']).unwrap_or(spec);
        if names.is_empty() {
            return false;
        }
        names
            .split(',')
            .take_while(|name| !name.is_empty())
            .all(|name| self.name_is_valid(name))
    }

    /// The line upstream's validator prints itself before the refusal:
    /// only `kex_names_valid` does, naming the first unknown algorithm
    /// (openssh/kex-names.c:173-179, `%.100s`).
    pub(in crate::ssh) fn validator_note(self, spec: &str) -> Option<String> {
        if self != Self::Kex || spec.starts_with('-') {
            return None;
        }
        let names = spec.strip_prefix(['+', '^']).unwrap_or(spec);
        let bad = names
            .split(',')
            .take_while(|name| !name.is_empty())
            .find(|name| !self.name_is_valid(name))?;
        let shown: String = bad.chars().take(100).collect();
        Some(format!("Unsupported KEX algorithm \"{shown}\""))
    }

    /// One element: an exact known name, or for `HostKeyAlgorithms` a
    /// pattern that matches (positively or negatively) some known name
    /// (openssh/sshkey.c:360-395 `sshkey_names_valid2` with
    /// `allow_wildcard`).
    fn name_is_valid(self, name: &str) -> bool {
        let known = self.known_names();
        known.contains(&name)
            || (self == Self::HostKey
                && known
                    .iter()
                    .any(|alg| match_pattern_list(alg, name, false) != PatternListMatch::None))
    }
}

/// Why an algorithm list could not be assembled.
#[derive(Debug, Clone, Eq, PartialEq)]
pub(in crate::ssh) enum AssembleError {
    /// A `!`-negated element outside a `-` list, which upstream refuses
    /// (openssh/kex-names.c:301-305).
    NegatedPattern,
    /// Nothing matched (openssh/kex-names.c:318-323).
    Empty,
}

/// Builds the algorithm list a directive yields - a port of
/// `kex_assemble_names` (openssh/kex-names.c:243-329).
///
/// `spec` is the directive's value, `None` when no directive claimed the
/// slot; `defaults` is the comma list used when the directive is absent;
/// `all` is the comma list of every algorithm a pattern may expand to.
///
/// The lists stay comma strings, as upstream's do, because its loops treat
/// an empty element differently: `kex_names_cat` stops at one while the
/// final pattern loop walks past it, so `^a,,!b` is refused though `^a,,b`
/// assembles.
pub(in crate::ssh) fn assemble(
    spec: Option<&str>,
    defaults: &str,
    all: &str,
) -> Result<Vec<String>, AssembleError> {
    let Some(spec) = spec.filter(|s| !s.is_empty()) else {
        return Ok(split_list(defaults));
    };
    let list = if let Some(rest) = spec.strip_prefix('+') {
        names_cat(defaults, rest)
    } else if let Some(rest) = spec.strip_prefix('-') {
        // Filtering is the whole job here (openssh/kex-names.c:268-276).
        return Ok(split_list(&filter_list(defaults, rest, true)));
    } else if let Some(rest) = spec.strip_prefix('^') {
        names_cat(rest, defaults)
    } else {
        spec.to_owned()
    };
    let mut assembled = String::new();
    for pattern in list.split(',') {
        if pattern.starts_with('!') {
            return Err(AssembleError::NegatedPattern);
        }
        assembled = names_cat(&assembled, &filter_list(all, pattern, false));
    }
    if assembled.is_empty() {
        return Err(AssembleError::Empty);
    }
    Ok(split_list(&assembled))
}

/// The elements of a comma list built by [`assemble`], which never holds an
/// empty one once assembled.
fn split_list(list: &str) -> Vec<String> {
    list.split(',')
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .collect()
}

/// `a` followed by each name of `b` not already in `a`, stopping at an
/// empty element of `b` (openssh/kex-names.c:202-233 `kex_names_cat`). An
/// empty `a` yields `b` verbatim and an empty `b` yields `a`.
fn names_cat(a: &str, b: &str) -> String {
    if a.is_empty() {
        return b.to_owned();
    }
    let mut out = a.to_owned();
    for name in b.split(',').take_while(|name| !name.is_empty()) {
        if !has_alg(&out, name) {
            out.push(',');
            out.push_str(name);
        }
    }
    out
}

/// Whether `name` is in the comma list `list`, read up to its first empty
/// element as `match_list` does (openssh/kex-names.c `kex_has_any_alg`,
/// openssh/match.c:283-293).
fn has_alg(list: &str, name: &str) -> bool {
    list.split(',')
        .take_while(|alg| !alg.is_empty())
        .any(|alg| alg == name)
}

/// Keeps the elements of `proposal` that the pattern list `filter` matches,
/// or with `deny` those it does not (openssh/match.c:320-347
/// `filter_list`).
fn filter_list(proposal: &str, filter: &str, deny: bool) -> String {
    proposal
        .split(',')
        .filter(|name| {
            let positive = match_pattern_list(name, filter, false) == PatternListMatch::Positive;
            positive != deny
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// Composes a directive's list against the transport's `defaults` and
/// keeps the names the transport can negotiate, in order.
///
/// Composition failures are upstream's: `fill_default_options` reports
/// `<name>: invalid argument` for the three transport lists
/// (openssh/readconf.c:2986-2997), and `HostKeyAlgorithms` fails as
/// `expand HostKeyAlgorithms` (openssh/sshconnect2.c:245-247). A name
/// OpenSSH knows but `map` rejects is left out with a warning naming it;
/// upstream would still offer it, so a list left with nothing the
/// transport supports is refused rather than sent empty.
pub(super) fn negotiable<T>(
    list: AlgorithmList,
    spec: &str,
    defaults: &str,
    map: impl Fn(&str) -> Option<T>,
) -> Result<Vec<T>, SshError> {
    let all = list.known_names().join(",");
    let names = assemble(Some(spec), defaults, &all).map_err(|_| SshError::SshConfigFatal {
        reason: format!("{}: invalid argument", list.assembly_name()),
    })?;
    let mut supported = Vec::with_capacity(names.len());
    for name in &names {
        match map(name) {
            Some(alg) => supported.push(alg),
            None => warn_log!(
                "{}: dropping {name}, which the embedded ssh transport cannot negotiate",
                list.directive()
            ),
        }
    }
    if supported.is_empty() {
        return Err(SshError::SshConfigFatal {
            reason: format!(
                "{} '{spec}': no algorithm the embedded ssh transport supports",
                list.directive()
            ),
        });
    }
    Ok(supported)
}

/// OpenSSH 10.0p1's client defaults, filtered to its own known names as
/// `fill_default_options` does (openssh/readconf.c:2976-2985,
/// openssh/myproposal.h); `HostKeyAlgorithms` is expanded later, at dump
/// and connect time (openssh/readconf.c:3630-3632,
/// openssh/sshconnect2.c:244-247). Only the oracle tests compare against them: the
/// embedded transport composes against its own defaults.
#[cfg(test)]
pub(in crate::ssh) fn openssh_defaults(list: AlgorithmList) -> String {
    let names: &[&str] = match list {
        AlgorithmList::Ciphers => &[
            "chacha20-poly1305@openssh.com",
            "aes128-gcm@openssh.com",
            "aes256-gcm@openssh.com",
            "aes128-ctr",
            "aes192-ctr",
            "aes256-ctr",
        ],
        AlgorithmList::Macs => &[
            "umac-64-etm@openssh.com",
            "umac-128-etm@openssh.com",
            "hmac-sha2-256-etm@openssh.com",
            "hmac-sha2-512-etm@openssh.com",
            "hmac-sha1-etm@openssh.com",
            "umac-64@openssh.com",
            "umac-128@openssh.com",
            "hmac-sha2-256",
            "hmac-sha2-512",
            "hmac-sha1",
        ],
        AlgorithmList::Kex => &[
            "mlkem768x25519-sha256",
            "sntrup761x25519-sha512",
            "sntrup761x25519-sha512@openssh.com",
            "curve25519-sha256",
            "curve25519-sha256@libssh.org",
            "ecdh-sha2-nistp256",
            "ecdh-sha2-nistp384",
            "ecdh-sha2-nistp521",
            "diffie-hellman-group-exchange-sha256",
            "diffie-hellman-group16-sha512",
            "diffie-hellman-group18-sha512",
            "diffie-hellman-group14-sha256",
        ],
        AlgorithmList::HostKey => &[
            "ssh-ed25519-cert-v01@openssh.com",
            "ecdsa-sha2-nistp256-cert-v01@openssh.com",
            "ecdsa-sha2-nistp384-cert-v01@openssh.com",
            "ecdsa-sha2-nistp521-cert-v01@openssh.com",
            "sk-ssh-ed25519-cert-v01@openssh.com",
            "sk-ecdsa-sha2-nistp256-cert-v01@openssh.com",
            "rsa-sha2-512-cert-v01@openssh.com",
            "rsa-sha2-256-cert-v01@openssh.com",
            "ssh-ed25519",
            "ecdsa-sha2-nistp256",
            "ecdsa-sha2-nistp384",
            "ecdsa-sha2-nistp521",
            "sk-ssh-ed25519@openssh.com",
            "sk-ecdsa-sha2-nistp256@openssh.com",
            "rsa-sha2-512",
            "rsa-sha2-256",
        ],
    };
    names.join(",")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(names: &[&str]) -> Vec<String> {
        names.iter().map(|&n| n.to_owned()).collect()
    }

    /// The validators' rules, each confirmed on `ssh -G`: exact names only
    /// for ciphers, wildcards for host keys, `-` unchecked, and the stop
    /// at an empty element.
    #[test]
    fn validation_follows_the_readconf_arms() {
        let c = AlgorithmList::Ciphers;
        assert!(c.spec_is_valid("aes128-ctr,aes256-ctr"));
        assert!(c.spec_is_valid("+aes128-cbc"));
        assert!(c.spec_is_valid("^aes256-ctr"));
        assert!(c.spec_is_valid("-bogus"));
        assert!(c.spec_is_valid("aes128-ctr,,bogus"));
        assert!(!c.spec_is_valid("aes128-ctr,bogus"));
        assert!(!c.spec_is_valid("aes*"));
        assert!(!c.spec_is_valid("+"));
        assert!(AlgorithmList::HostKey.spec_is_valid("ssh-ed25519*"));
        assert!(AlgorithmList::HostKey.spec_is_valid("!ssh-rsa"));
        assert!(!AlgorithmList::HostKey.spec_is_valid("nope"));
        assert!(!AlgorithmList::Macs.spec_is_valid("hmac-sha3"));
        assert!(AlgorithmList::Kex.spec_is_valid("mlkem768x25519-sha256"));
    }

    /// `kex_assemble_names`, operator by operator, against a small default.
    #[test]
    fn assembly_mirrors_kex_assemble_names() {
        let (def, all) = ("b,c", "a,b,c,d");
        let ok = |names: &[&str]| Ok(strings(names));
        assert_eq!(assemble(None, def, all), ok(&["b", "c"]));
        assert_eq!(assemble(Some("d,a"), def, all), ok(&["d", "a"]));
        assert_eq!(assemble(Some("+a"), def, all), ok(&["b", "c", "a"]));
        assert_eq!(assemble(Some("^c,a"), def, all), ok(&["c", "a", "b"]));
        assert_eq!(assemble(Some("-b"), def, all), ok(&["c"]));
        assert_eq!(assemble(Some("-*"), def, all), ok(&[]));
        assert_eq!(assemble(Some("*"), def, all), ok(&["a", "b", "c", "d"]));
        assert_eq!(assemble(Some("x"), def, all), Err(AssembleError::Empty));
        assert_eq!(
            assemble(Some("a,!b"), def, all),
            Err(AssembleError::NegatedPattern)
        );
        // A pattern appended with `+` expands against every known name.
        assert_eq!(assemble(Some("+?"), def, all), ok(&["b", "c", "a", "d"]));
        // `kex_names_cat` stops at an empty element; the pattern loop
        // does not, so a negation behind one is still refused.
        assert_eq!(assemble(Some("+a,,d"), def, all), ok(&["b", "c", "a"]));
        assert_eq!(assemble(Some("^a,,d"), def, all), ok(&["a", "d", "b", "c"]));
        assert_eq!(
            assemble(Some("^a,,!d"), def, all),
            Err(AssembleError::NegatedPattern)
        );
    }
}

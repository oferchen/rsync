//! The single ssh_config option table, shared by both config readers.
//!
//! oc has two consumers of `~/.ssh/config`, split by what they need from
//! it rather than by how they read it:
//!
//! * [`crate::ssh::config_lookup`] resolves one boolean - whether
//!   `Compression yes` is in effect - to warn about double compression.
//!   It honours `Host` and `Match` blocks and never fails.
//! * `crate::ssh::embedded::ssh_config` resolves the connection
//!   parameters the embedded transport acts on (`HostName`, `User`,
//!   `Port`, `IdentityFile`, `IdentitiesOnly`, `IdentityAgent`). It
//!   honours `Host` blocks and refuses a file real `ssh` would refuse.
//!
//! Before this module each carried its own keyword set, its own
//! `Key Value` splitter, its own yes/no parse and its own glob matcher, so
//! every keyword added to one had to be added to the other by hand. This
//! module is the one place a keyword is declared, mirroring upstream's
//! shape: a `name -> opcode` table read by one lookup, with the per-option
//! value handling selected by the opcode.
//!
//! upstream: openssh/readconf.c:175-325 `keywords[]`, openssh/readconf.c:132
//! `OpCodes`, openssh/readconf.c:958 `parse_token()`.
//!
//! # Scope
//!
//! The table holds every keyword upstream recognises
//! (openssh/readconf.c:192-337), so a valid `ssh -G` config is a valid oc
//! config. Keywords oc acts on carry their own opcode; the rest keep
//! upstream's accept-and-ignore categories ([`Opcode::Ignore`],
//! [`Opcode::Deprecated`], [`Opcode::Unsupported`]) or, for keywords upstream
//! implements and the embedded transport does not, [`Opcode::NotActedOn`].
//! A spelling outside the table resolves to [`Opcode::Unknown`], upstream's
//! `oBadOption`, which the embedded reader counts and then refuses the file
//! on (openssh/readconf.c:2604-2613).
//!
//! # What this module does NOT own
//!
//! Which opcodes a reader acts on, and what it does with the value, stay
//! with that reader: the two have deliberately different failure policies
//! and deliberately different scopes. The table answers "what keyword is
//! this and how is its value shaped", never "what should happen next".

/// One ssh_config keyword, resolved from its spelling.
///
/// Mirrors upstream's `OpCodes` (openssh/readconf.c:146-185): one variant
/// per keyword oc acts on, upstream's three accept-and-ignore categories,
/// one variant for the keywords upstream implements and oc does not act on,
/// and the `oBadOption` catch-all. `oIgnoredUnknownOption` has no variant
/// because it depends on the `IgnoreUnknown` list in force at the line, so
/// the reader decides it, as upstream's `parse_token` does
/// (openssh/readconf.c:977-979).
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(in crate::ssh) enum Opcode {
    /// `Host <patterns>` - opens a block gated on the target alias.
    /// upstream: openssh/readconf.c:260 `oHost`, arm at :1827.
    Host,
    /// `Match <criteria>` - opens a conditionally active block.
    /// upstream: openssh/readconf.c:261 `oMatch`, arm at :1868.
    Match,
    /// `Include <pattern>...` - reads the glob-expanded file(s) inline.
    /// upstream: openssh/readconf.c:298 `oInclude`, arm at :2066.
    Include,
    /// `Compression yes|no`.
    /// upstream: openssh/readconf.c:270 `oCompression`, arm at :1345.
    Compression,
    /// `HostName <name>`.
    /// upstream: openssh/readconf.c:250 `oHostname`, arm at :1494.
    Hostname,
    /// `User <name>`.
    /// upstream: openssh/readconf.c:259 `oUser`, arm at :1440.
    User,
    /// `Port <number>`.
    /// upstream: openssh/readconf.c:253 `oPort`, arm at :1563.
    Port,
    /// `IdentityFile <path>`.
    /// upstream: openssh/readconf.c:244 `oIdentityFile`, arm at :1395.
    IdentityFile,
    /// `IdentitiesOnly yes|no`.
    /// upstream: openssh/readconf.c:246 `oIdentitiesOnly`, arm at :1918.
    IdentitiesOnly,
    /// `IdentityAgent <path>`.
    /// upstream: openssh/readconf.c:249 `oIdentityAgent`, arm at :2381.
    IdentityAgent,
    /// `ConnectTimeout <time>|none`.
    /// upstream: openssh/readconf.c:288 `oConnectTimeout`, arm at :1216.
    ConnectTimeout,
    /// `AddressFamily any|inet|inet6`.
    /// upstream: openssh/readconf.c:289 `oAddressFamily`, arm at :1909 via
    /// `multistate_addressfamily` (:1038).
    AddressFamily,
    /// `BindAddress <addr>`.
    /// upstream: openssh/readconf.c:281 `oBindAddress`, arm at :1510.
    BindAddress,
    /// `BindInterface <iface>`.
    /// upstream: openssh/readconf.c:282 `oBindInterface`, arm at :1514.
    BindInterface,
    /// `ConnectionAttempts <count>`.
    /// upstream: openssh/readconf.c:266 `oConnectionAttempts`, arm at :1580.
    ConnectionAttempts,
    /// `IPQoS <interactive> [bulk]`.
    /// upstream: openssh/readconf.c:306 `oIPQoS`, arm at :2154.
    IPQoS,
    /// `TCPKeepAlive yes|no|transport|all` (obsolete alias `KeepAlive`).
    /// upstream: openssh/readconf.c:271 `oTCPKeepAlive`, arm at :1350 via
    /// `multistate_keepalives` (:1080).
    TCPKeepAlive,
    /// `ServerAliveInterval <time>`.
    /// upstream: openssh/readconf.c:290 `oServerAliveInterval`, arm at :1922.
    ServerAliveInterval,
    /// `ServerAliveCountMax <count>`.
    /// upstream: openssh/readconf.c:291 `oServerAliveCountMax`, arm at :1926.
    ServerAliveCountMax,
    /// `ProxyCommand <command>` - the rest of the line taken verbatim and
    /// later run through a shell, its stdio replacing the direct socket.
    /// upstream: openssh/readconf.c:252 `oProxyCommand`, arm at :1530
    /// (`parse_command`); dialled by openssh/sshconnect.c:222
    /// `ssh_proxy_connect`.
    ProxyCommand,
    /// `ProxyJump [user@]host[:port][,...]` - a jump-host chain upstream
    /// synthesises into an equivalent `ProxyCommand`.
    /// upstream: openssh/readconf.c:327 `oProxyJump`, arm at :1547
    /// (`parse_jump`); the synthesis is openssh/ssh.c:1310-1360.
    ProxyJump,
    /// `ProxyUseFdpass yes|no` - whether the `ProxyCommand` passes back a
    /// connected file descriptor rather than piping through stdio.
    /// upstream: openssh/readconf.c:311 `oProxyUseFdpass`, arm at :2197.
    ProxyUseFdpass,
    /// `UserKnownHostsFile <file>...` - the ordered list of per-user
    /// known_hosts files consulted for host-key verification and appended
    /// to when a new key is learned. `none` (alone) disables the user list.
    /// upstream: openssh/readconf.c:265 `oUserKnownHostsFile`, arm at :1488
    /// via the `parse_char_array` path (:1457-1486).
    UserKnownHostsFile,
    /// `GlobalKnownHostsFile <file>...` - the ordered list of system-wide
    /// known_hosts files, consulted for verification only (never written).
    /// `none` (alone) disables the system list.
    /// upstream: openssh/readconf.c:264 `oGlobalKnownHostsFile`, arm at
    /// :1453 via the same `parse_char_array` path.
    GlobalKnownHostsFile,
    /// `HashKnownHosts yes|no` - whether a newly learned host entry is
    /// written hashed (`|1|salt|hash`) rather than as a plaintext hostname.
    /// upstream: openssh/readconf.c:297 `oHashKnownHosts`, arm at :2022 via
    /// `parse_flag`.
    HashKnownHosts,
    /// `HostKeyAlias <alias>` - the name used to look up and store the host
    /// key instead of the real hostname. Taken verbatim, never expanded.
    /// upstream: openssh/readconf.c:251 `oHostKeyAlias`, arm at :1502 via
    /// `parse_string`.
    HostKeyAlias,
    /// `CheckHostIP yes|no` - whether the host key is additionally checked
    /// against the server's resolved IP address.
    /// upstream: openssh/readconf.c:268 `oCheckHostIP`, arm at :1331 via
    /// `parse_flag`.
    CheckHostIP,
    /// `RevokedHostKeys <file>` - a file of revoked host keys; a server key
    /// listed there is rejected. Path taken verbatim (tilde-expanded by the
    /// consumer).
    /// upstream: openssh/readconf.c:319 `oRevokedHostKeys`, arm at :2314 via
    /// `parse_string`.
    RevokedHostKeys,
    /// `PubkeyAuthentication yes|no|unbound|host-bound` - whether public-key
    /// authentication (agent and identity files alike) may be attempted.
    /// A multistate whose `unbound`/`host-bound` rows select the
    /// session-bound-pubkey extension the embedded transport does not
    /// negotiate, so both collapse onto "public-key enabled" here.
    /// upstream: openssh/readconf.c:241 `oPubkeyAuthentication`, arm at
    /// :1310 via `multistate_pubkey_auth` (:1086-1094).
    PubkeyAuthentication,
    /// `PasswordAuthentication yes|no` - whether password authentication may
    /// be attempted.
    /// upstream: openssh/readconf.c:235 `oPasswordAuthentication`, arm at
    /// :1298 (`parse_flag`).
    PasswordAuthentication,
    /// `KbdInteractiveAuthentication yes|no` (aliases
    /// `ChallengeResponseAuthentication`, `SKeyAuthentication`,
    /// `TISAuthentication`) - whether keyboard-interactive authentication may
    /// be attempted.
    /// upstream: openssh/readconf.c:236 `oKbdInteractiveAuthentication`, arm
    /// at :1302 (`parse_flag`); the three aliases are :238-240.
    KbdInteractiveAuthentication,
    /// `PreferredAuthentications <comma-list>` - the order the client offers
    /// authentication methods in, most preferred first.
    /// upstream: openssh/readconf.c:278 `oPreferredAuthentications`, arm at
    /// :1506 (`parse_string`).
    PreferredAuthentications,
    /// `BatchMode yes|no` - when on, never prompt interactively for a
    /// password or passphrase.
    /// upstream: openssh/readconf.c:267 `oBatchMode`, arm at :1327
    /// (`parse_flag`).
    BatchMode,
    /// `NumberOfPasswordPrompts <int>` - how many times a password may be
    /// prompted for before the method is abandoned.
    /// upstream: openssh/readconf.c:273 `oNumberOfPasswordPrompts`, arm at
    /// :1358 (`parse_int`).
    NumberOfPasswordPrompts,
    /// `CertificateFile <path>` - a certificate to offer alongside a key;
    /// accumulates across active lines.
    /// upstream: openssh/readconf.c:247 `oCertificateFile`, arm at :1415
    /// (`add_certificate_file`).
    CertificateFile,
    /// `RequiredRSASize <int>` - the minimum accepted RSA key length in bits.
    /// upstream: openssh/readconf.c:330 `oRequiredRSASize`, arm at :2412
    /// (`parse_int`).
    RequiredRSASize,
    /// `Ciphers [+-^]<list>` - the cipher preference.
    /// upstream: openssh/readconf.c:254 `oCiphers`, arm at :1593.
    Ciphers,
    /// `MACs [+-^]<list>` - the MAC preference.
    /// upstream: openssh/readconf.c:255 `oMacs`, arm at :1610.
    Macs,
    /// `KexAlgorithms [+-^]<list>` - the key-exchange preference.
    /// upstream: openssh/readconf.c:305 `oKexAlgorithms`, arm at :1627.
    KexAlgorithms,
    /// `HostKeyAlgorithms [+-^]<list>` - the host-key algorithm preference.
    /// upstream: openssh/readconf.c:279 `oHostKeyAlgorithms`, arm at :1645.
    HostKeyAlgorithms,
    /// `RekeyLimit <bytes>|default [<time>|none]` - when to re-key.
    /// upstream: openssh/readconf.c:287 `oRekeyLimit`, arm at :1362.
    RekeyLimit,
    /// `IgnoreUnknown <pattern-list>` - unknown keywords matching the list
    /// are skipped instead of refused, from the line after it onwards.
    /// upstream: openssh/readconf.c:326 `oIgnoreUnknown`, arm at :2193
    /// (`parse_string`); read by `parse_token` (openssh/readconf.c:977-979).
    IgnoreUnknown,
    /// `Protocol` - accepted and ignored without a word.
    /// upstream: openssh/readconf.c:194 `oIgnore`, arm at :1208.
    Ignore,
    /// A keyword upstream retired; accepted with a debug-level note.
    /// upstream: openssh/readconf.c:195-202 `oDeprecated`, arm at :2511.
    Deprecated,
    /// A keyword upstream no longer supports; accepted after an error-level
    /// note, without counting as a bad option.
    /// upstream: openssh/readconf.c:205-210 `oUnsupported`, arm at :2517.
    Unsupported,
    /// A keyword upstream implements and the embedded transport does not act
    /// on: forwarding, session shaping, interactive-terminal options, local
    /// command execution, `RemoteCommand` (which would replace
    /// `rsync --server`), and the options later rows of this table will
    /// take over. Accepted so a config `ssh` loads, loads here too; its
    /// value is not validated.
    NotActedOn,
    /// A spelling outside the table - upstream's `oBadOption`
    /// (openssh/readconf.c:147, returned by `parse_token` at :980-982).
    Unknown,
}

/// The option table: the ONE place an ssh_config keyword is declared.
///
/// Spellings are lower case and matched case-insensitively, because
/// upstream lowercases the keyword before its `strcmp`
/// (openssh/readconf.c:1184 `lowercase(keyword)` into
/// openssh/readconf.c:964-966).
const KEYWORDS: &[(&str, Opcode)] = &[
    ("addkeystoagent", Opcode::NotActedOn),
    ("addressfamily", Opcode::AddressFamily),
    ("afstokenpassing", Opcode::Unsupported),
    ("batchmode", Opcode::BatchMode),
    ("bindaddress", Opcode::BindAddress),
    ("bindinterface", Opcode::BindInterface),
    ("canonicaldomains", Opcode::NotActedOn),
    ("canonicalizefallbacklocal", Opcode::NotActedOn),
    ("canonicalizehostname", Opcode::NotActedOn),
    ("canonicalizemaxdots", Opcode::NotActedOn),
    ("canonicalizepermittedcnames", Opcode::NotActedOn),
    ("casignaturealgorithms", Opcode::NotActedOn),
    ("certificatefile", Opcode::CertificateFile),
    // Alias resolving to the same opcode as `KbdInteractiveAuthentication`.
    // upstream: openssh/readconf.c:238 `{ "challengeresponseauthentication",
    // oKbdInteractiveAuthentication }`.
    (
        "challengeresponseauthentication",
        Opcode::KbdInteractiveAuthentication,
    ),
    ("channeltimeout", Opcode::NotActedOn),
    ("checkhostip", Opcode::CheckHostIP),
    ("cipher", Opcode::Deprecated),
    ("ciphers", Opcode::Ciphers),
    ("clearallforwardings", Opcode::NotActedOn),
    ("compression", Opcode::Compression),
    ("compressionlevel", Opcode::Unsupported),
    ("connectionattempts", Opcode::ConnectionAttempts),
    ("connecttimeout", Opcode::ConnectTimeout),
    ("controlmaster", Opcode::NotActedOn),
    ("controlpath", Opcode::NotActedOn),
    ("controlpersist", Opcode::NotActedOn),
    // Alias resolving to the same opcode as `PubkeyAuthentication`.
    // upstream: openssh/readconf.c:242 `{ "dsaauthentication",
    // oPubkeyAuthentication }`.
    ("dsaauthentication", Opcode::PubkeyAuthentication),
    ("dynamicforward", Opcode::NotActedOn),
    ("enableescapecommandline", Opcode::NotActedOn),
    ("enablesshkeysign", Opcode::NotActedOn),
    ("escapechar", Opcode::NotActedOn),
    ("exitonforwardfailure", Opcode::NotActedOn),
    ("fallbacktorsh", Opcode::Deprecated),
    ("fingerprinthash", Opcode::NotActedOn),
    ("forkafterauthentication", Opcode::NotActedOn),
    ("forwardagent", Opcode::NotActedOn),
    ("forwardx11", Opcode::NotActedOn),
    ("forwardx11timeout", Opcode::NotActedOn),
    ("forwardx11trusted", Opcode::NotActedOn),
    ("gatewayports", Opcode::NotActedOn),
    ("globalknownhostsfile", Opcode::GlobalKnownHostsFile),
    ("globalknownhostsfile2", Opcode::Deprecated),
    ("gssapiauthentication", Opcode::NotActedOn),
    ("gssapidelegatecredentials", Opcode::NotActedOn),
    ("hashknownhosts", Opcode::HashKnownHosts),
    ("host", Opcode::Host),
    ("hostbasedacceptedalgorithms", Opcode::NotActedOn),
    ("hostbasedauthentication", Opcode::NotActedOn),
    ("hostbasedkeytypes", Opcode::NotActedOn),
    ("hostkeyalgorithms", Opcode::HostKeyAlgorithms),
    ("hostkeyalias", Opcode::HostKeyAlias),
    ("hostname", Opcode::Hostname),
    ("identitiesonly", Opcode::IdentitiesOnly),
    ("identityagent", Opcode::IdentityAgent),
    ("identityfile", Opcode::IdentityFile),
    ("identityfile2", Opcode::IdentityFile),
    ("ignoreunknown", Opcode::IgnoreUnknown),
    ("include", Opcode::Include),
    ("ipqos", Opcode::IPQoS),
    (
        "kbdinteractiveauthentication",
        Opcode::KbdInteractiveAuthentication,
    ),
    ("kbdinteractivedevices", Opcode::NotActedOn),
    // Obsolete alias resolving to the same opcode as `TCPKeepAlive`.
    // upstream: openssh/readconf.c:272 `{ "keepalive", oTCPKeepAlive }`.
    ("keepalive", Opcode::TCPKeepAlive),
    ("kerberosauthentication", Opcode::Unsupported),
    ("kerberostgtpassing", Opcode::Unsupported),
    ("kexalgorithms", Opcode::KexAlgorithms),
    ("knownhostscommand", Opcode::NotActedOn),
    ("localcommand", Opcode::NotActedOn),
    ("localforward", Opcode::NotActedOn),
    ("loglevel", Opcode::NotActedOn),
    ("logverbose", Opcode::NotActedOn),
    ("macs", Opcode::Macs),
    ("match", Opcode::Match),
    ("nohostauthenticationforlocalhost", Opcode::NotActedOn),
    ("numberofpasswordprompts", Opcode::NumberOfPasswordPrompts),
    ("obscurekeystroketiming", Opcode::NotActedOn),
    ("passwordauthentication", Opcode::PasswordAuthentication),
    ("permitlocalcommand", Opcode::NotActedOn),
    ("permitremoteopen", Opcode::NotActedOn),
    ("pkcs11provider", Opcode::NotActedOn),
    ("port", Opcode::Port),
    ("preferredauthentications", Opcode::PreferredAuthentications),
    ("protocol", Opcode::Ignore),
    ("proxycommand", Opcode::ProxyCommand),
    ("proxyjump", Opcode::ProxyJump),
    ("proxyusefdpass", Opcode::ProxyUseFdpass),
    ("pubkeyacceptedalgorithms", Opcode::NotActedOn),
    ("pubkeyacceptedkeytypes", Opcode::NotActedOn),
    ("pubkeyauthentication", Opcode::PubkeyAuthentication),
    ("rekeylimit", Opcode::RekeyLimit),
    ("remotecommand", Opcode::NotActedOn),
    ("remoteforward", Opcode::NotActedOn),
    ("requesttty", Opcode::NotActedOn),
    ("requiredrsasize", Opcode::RequiredRSASize),
    ("revokedhostkeys", Opcode::RevokedHostKeys),
    ("rhostsauthentication", Opcode::Deprecated),
    ("rhostsrsaauthentication", Opcode::Unsupported),
    ("rsaauthentication", Opcode::Unsupported),
    ("securitykeyprovider", Opcode::NotActedOn),
    ("sendenv", Opcode::NotActedOn),
    ("serveralivecountmax", Opcode::ServerAliveCountMax),
    ("serveraliveinterval", Opcode::ServerAliveInterval),
    ("sessiontype", Opcode::NotActedOn),
    ("setenv", Opcode::NotActedOn),
    // Alias resolving to the same opcode as `KbdInteractiveAuthentication`.
    // upstream: openssh/readconf.c:239 `{ "skeyauthentication",
    // oKbdInteractiveAuthentication }`.
    ("skeyauthentication", Opcode::KbdInteractiveAuthentication),
    ("smartcarddevice", Opcode::NotActedOn),
    ("stdinnull", Opcode::NotActedOn),
    ("streamlocalbindmask", Opcode::NotActedOn),
    ("streamlocalbindunlink", Opcode::NotActedOn),
    ("stricthostkeychecking", Opcode::NotActedOn),
    ("syslogfacility", Opcode::NotActedOn),
    ("tag", Opcode::NotActedOn),
    ("tcpkeepalive", Opcode::TCPKeepAlive),
    // Alias resolving to the same opcode as `KbdInteractiveAuthentication`.
    // upstream: openssh/readconf.c:240 `{ "tisauthentication",
    // oKbdInteractiveAuthentication }`.
    ("tisauthentication", Opcode::KbdInteractiveAuthentication),
    ("tunnel", Opcode::NotActedOn),
    ("tunneldevice", Opcode::NotActedOn),
    ("updatehostkeys", Opcode::NotActedOn),
    ("useprivilegedport", Opcode::Deprecated),
    ("user", Opcode::User),
    ("userknownhostsfile", Opcode::UserKnownHostsFile),
    ("userknownhostsfile2", Opcode::Deprecated),
    ("useroaming", Opcode::Deprecated),
    ("usersh", Opcode::Deprecated),
    ("verifyhostkeydns", Opcode::NotActedOn),
    ("versionaddendum", Opcode::NotActedOn),
    ("visualhostkey", Opcode::NotActedOn),
    ("xauthlocation", Opcode::NotActedOn),
];

/// Resolves a keyword spelling to its [`Opcode`], case-insensitively.
///
/// upstream: openssh/readconf.c:958-973 `parse_token()`, which scans the
/// same flat table and returns `oBadOption` for anything it does not find.
pub(in crate::ssh) fn parse_token(keyword: &str) -> Opcode {
    KEYWORDS
        .iter()
        .find(|(name, _)| keyword.eq_ignore_ascii_case(name))
        .map_or(Opcode::Unknown, |(_, opcode)| *opcode)
}

/// How a keyword's value half is shaped, which is what decides the parse
/// and the wording of a missing-value refusal.
///
/// Upstream expresses this as the arm each opcode `goto`s into; oc names it
/// so the two readers cannot disagree about which shape a keyword has.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(in crate::ssh) enum ValueKind {
    /// A pattern list walked by the `oHost` arm itself
    /// (openssh/readconf.c:1831-1858). There is no missing-value refusal:
    /// the walk simply runs zero times and leaves the block inactive.
    HostPatterns,
    /// A `Match` criteria list, walked by upstream's `match_cfg_line`
    /// (openssh/readconf.c:1864-1870).
    MatchCriteria,
    /// A whitespace-separated list of glob patterns, each read as an
    /// additional config file inline. upstream: the `oInclude` arm's
    /// `while (argv_next(...))` loop (openssh/readconf.c:2079).
    IncludePatterns,
    /// A whitespace-separated list of known_hosts file paths, with `none`
    /// permitted only in first position and alone. upstream: the
    /// `parse_char_array` path shared by `oUserKnownHostsFile` and
    /// `oGlobalKnownHostsFile` (openssh/readconf.c:1600-1652). Only the
    /// FIRST active line of a given keyword claims the list; a later line is
    /// ignored (`value = *uintptr == 0` gate, openssh/readconf.c:1603,
    /// :1646), so the cross-line policy is first-obtained even though the
    /// one claiming line accumulates every token on it.
    KnownHostsFiles,
    /// One token, read as a multistate yes/no flag.
    /// upstream: `parse_multistate_value` (openssh/readconf.c:1103-1117).
    Flag,
    /// One token, taken verbatim - a string, a path, or a port number.
    /// upstream: the `parse_string` family (openssh/readconf.c:1441-1445).
    Single,
    /// One token, read as a time value by `convtime`, with `none` as the
    /// unset sentinel. upstream: the `parse_time` arm
    /// (openssh/readconf.c:1215-1233).
    Time,
    /// One token, read as a non-negative integer by `atoi_err`
    /// (openssh/misc.c:2448-2456, `strtonum(arg, 0, INT_MAX)`).
    /// upstream: the `parse_int` arm (openssh/readconf.c:1576-1587).
    Int,
    /// One token, read as an address-family multistate (`any`, `inet`,
    /// `inet6`). upstream: the `parse_multistate` arm
    /// (openssh/readconf.c:1264-1276) with `multistate_addressfamily`
    /// (openssh/readconf.c:1038-1043).
    AddressFamily,
    /// One token, read as a keepalive multistate (`yes`/`true`/`transport`,
    /// `no`/`false`, `all`). upstream: the `parse_multistate` arm with
    /// `multistate_keepalives` (openssh/readconf.c:1080-1088).
    KeepAlive,
    /// One token, read as a public-key-auth multistate
    /// (`yes`/`true`/`no`/`false`/`unbound`/`host-bound`). upstream: the
    /// `parse_multistate` arm with `multistate_pubkey_auth`
    /// (openssh/readconf.c:1086-1094).
    PubkeyAuth,
    /// A byte count (or `default`) and an optional time (or `none`).
    /// upstream: the `oRekeyLimit` arm (openssh/readconf.c:1362-1393).
    RekeyLimit,
    /// One or two tokens, each an IPQoS class or DSCP value.
    /// upstream: the two-token `oIPQoS` arm (openssh/readconf.c:2148-2170).
    IpQos,
    /// The rest of the line taken verbatim, un-tokenised - the shape
    /// `ProxyCommand` and `ProxyJump` share. Upstream's `parse_command`
    /// arm reads `s + strspn(s, WHITESPACE "=")` rather than a tokenised
    /// arg (openssh/readconf.c:1465-1470), and `parse_jump` likewise acts
    /// on the whole remaining string `s` (openssh/readconf.c:1730-1735).
    Command,
    /// No shape, because no reader parses the keyword's value.
    Unknown,
}

/// How repeated assignments to one keyword resolve across the single
/// ordered scan of a config file.
///
/// Upstream has no such enum; the split is expressed by which code each
/// arm runs. Most options are a scalar slot assigned only while unset
/// (openssh/readconf.c:1229 `if (*activep && *intptr == -1)`), so the
/// FIRST value obtained from an active line wins and every later
/// assignment is ignored. A few list options append on every active line
/// instead - `IdentityFile` routes through `add_identity_file` with no
/// unset test at all (openssh/readconf.c:1394).
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
#[cfg_attr(not(feature = "ssh-config-parse"), allow(dead_code))]
pub(in crate::ssh) enum ResolutionPolicy {
    /// One slot; the first value obtained from an active line claims it.
    FirstObtained,
    /// A list; every active line appends.
    Accumulate,
}

impl ResolutionPolicy {
    /// Whether a directive under this policy may write its value now.
    /// `slot_claimed` is whether an earlier active line already set the
    /// option - upstream's `*intptr != -1` state.
    #[cfg_attr(not(feature = "ssh-config-parse"), allow(dead_code))]
    pub(in crate::ssh) fn may_assign(self, slot_claimed: bool) -> bool {
        match self {
            Self::FirstObtained => !slot_claimed,
            Self::Accumulate => true,
        }
    }
}

impl Opcode {
    /// How repeated assignments to this keyword resolve. `IdentityFile`
    /// is the one accumulating row oc reads today (upstream's other
    /// accumulators, e.g. `CertificateFile`, join here when added).
    #[cfg_attr(not(feature = "ssh-config-parse"), allow(dead_code))]
    pub(in crate::ssh) fn resolution_policy(self) -> ResolutionPolicy {
        match self {
            // `IdentityFile` and `CertificateFile` are the accumulating rows:
            // both append on every active line with no unset test
            // (openssh/readconf.c:1364 `add_identity_file`, :1385
            // `add_certificate_file`).
            Self::IdentityFile | Self::CertificateFile => ResolutionPolicy::Accumulate,
            _ => ResolutionPolicy::FirstObtained,
        }
    }

    /// The value shape this keyword's arm expects.
    pub(in crate::ssh) fn value_kind(self) -> ValueKind {
        match self {
            Self::Host => ValueKind::HostPatterns,
            Self::Match => ValueKind::MatchCriteria,
            Self::Include => ValueKind::IncludePatterns,
            Self::Compression
            | Self::IdentitiesOnly
            | Self::ProxyUseFdpass
            | Self::HashKnownHosts
            | Self::CheckHostIP
            | Self::PasswordAuthentication
            | Self::KbdInteractiveAuthentication
            | Self::BatchMode => ValueKind::Flag,
            Self::PubkeyAuthentication => ValueKind::PubkeyAuth,
            Self::ProxyCommand | Self::ProxyJump => ValueKind::Command,
            Self::UserKnownHostsFile | Self::GlobalKnownHostsFile => ValueKind::KnownHostsFiles,
            Self::Hostname
            | Self::User
            | Self::Port
            | Self::IdentityFile
            | Self::IdentityAgent
            | Self::BindAddress
            | Self::BindInterface
            | Self::HostKeyAlias
            | Self::RevokedHostKeys
            | Self::CertificateFile
            | Self::PreferredAuthentications
            | Self::IgnoreUnknown
            | Self::Ciphers
            | Self::Macs
            | Self::KexAlgorithms
            | Self::HostKeyAlgorithms => ValueKind::Single,
            Self::RekeyLimit => ValueKind::RekeyLimit,
            Self::ConnectTimeout | Self::ServerAliveInterval => ValueKind::Time,
            Self::ConnectionAttempts
            | Self::ServerAliveCountMax
            | Self::NumberOfPasswordPrompts
            | Self::RequiredRSASize => ValueKind::Int,
            Self::AddressFamily => ValueKind::AddressFamily,
            Self::TCPKeepAlive => ValueKind::KeepAlive,
            Self::IPQoS => ValueKind::IpQos,
            Self::Ignore
            | Self::Deprecated
            | Self::Unsupported
            | Self::NotActedOn
            | Self::Unknown => ValueKind::Unknown,
        }
    }

    /// The diagnostic upstream prints when this keyword's `argv_next`
    /// yields nothing, or `None` for the shapes that have no such refusal.
    ///
    /// The capitalisation is upstream's own and splits by shape, not by
    /// keyword: the multistate flags route through
    /// `parse_multistate_value` and print `missing argument.`
    /// (openssh/readconf.c:1108) while the single-token arms print
    /// `Missing argument.` (openssh/readconf.c:1364, :1397, :1444, :1562,
    /// :2423) and the time arm prints `missing time value.`
    /// (openssh/readconf.c:1219-1220). Deriving it from [`ValueKind`] is
    /// what keeps a new keyword from picking the wrong one.
    #[cfg_attr(not(feature = "embedded-ssh"), allow(dead_code))]
    pub(in crate::ssh) fn missing_argument(self) -> Option<&'static str> {
        match self.value_kind() {
            // The multistate arms (`Flag`, `AddressFamily`, `KeepAlive`)
            // all route through `parse_multistate_value`, whose absent-value
            // wording is `missing argument.` (openssh/readconf.c:1106).
            ValueKind::Flag
            | ValueKind::AddressFamily
            | ValueKind::KeepAlive
            | ValueKind::PubkeyAuth => Some("missing argument."),
            ValueKind::Single | ValueKind::RekeyLimit => Some("Missing argument."),
            ValueKind::Time => Some("missing time value."),
            // `atoi_err(NULL)` returns `"missing"`, printed as
            // `integer value missing.` (openssh/readconf.c:1579,
            // openssh/misc.c:2452).
            ValueKind::Int => Some("integer value missing."),
            // `IpQos` refuses an absent value with a VALUE-interpolated
            // `Bad IPQoS value: %s` (openssh/readconf.c:2151), not a fixed
            // string, so it has no entry here - like the list shapes below,
            // whose refusal is the per-token empty check rather than an
            // absent-value one.
            // `ProxyCommand`/`ProxyJump` (the `Command` shape) never surface a
            // fixed missing-value diagnostic here: upstream's `parse_command`
            // silently leaves the slot unset when only the keyword is present
            // (openssh/readconf.c:1465-1470), and a keyword-only line is
            // dropped before dispatch because [`split_directive`] returns
            // `None` for it.
            ValueKind::IpQos
            | ValueKind::HostPatterns
            | ValueKind::MatchCriteria
            | ValueKind::IncludePatterns
            // The known_hosts file lists are list-shaped like `Include`: an
            // absent value walks the loop zero times rather than refusing,
            // and an empty TOKEN is the per-token `keyword %s empty argument`
            // (openssh/readconf.c:1626), not an absent-value diagnostic.
            | ValueKind::KnownHostsFiles
            | ValueKind::Command
            | ValueKind::Unknown => None,
        }
    }
}

/// Splits a `Key Value` (or `Key=Value`) line on the first run of
/// whitespace or `=`. Returns `None` when the line carries only a keyword.
///
/// The keyword half is upstream's `strdelim` (openssh/readconf.c:1176,
/// openssh/misc.c:469-507), a *different* splitter from `argv_split`; it is
/// the returned value half that a reader then tokenises with
/// [`crate::ssh::argv_split`].
pub(in crate::ssh) fn split_directive(line: &str) -> Option<(&str, &str)> {
    let (key, rest) = line.split_once(|c: char| c.is_whitespace() || c == '=')?;
    let value = rest.trim_start_matches(|c: char| c.is_whitespace() || c == '=');
    if value.is_empty() {
        return None;
    }
    Some((key, value))
}

/// Parses a [`ValueKind::Flag`] value. Returns `None` for anything outside
/// the multistate set so a typo cannot silently flip a setting.
///
/// upstream: `multistate_flag` (openssh/readconf.c:994-1000) accepts
/// `true`, `false`, `yes` and `no`, compared with `strcasecmp`
/// (openssh/readconf.c:1113).
pub(in crate::ssh) fn parse_flag_value(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "yes" | "true" => Some(true),
        "no" | "false" => Some(false),
        _ => None,
    }
}

/// Parses a [`ValueKind::PubkeyAuth`] value into "public-key auth enabled".
/// Returns `None` for anything outside the multistate set so a typo cannot
/// silently flip the setting.
///
/// upstream: `multistate_pubkey_auth` (openssh/readconf.c:1086-1094) accepts
/// `true`/`yes` (`SSH_PUBKEY_AUTH_ALL`), `false`/`no` (`SSH_PUBKEY_AUTH_NO`)
/// and the session-bound-key selectors `unbound`/`host-bound`, all compared
/// with `strcasecmp` (openssh/readconf.c:1109-1112). The embedded transport
/// negotiates neither bound-key form, so `unbound` and `host-bound` collapse
/// onto "enabled" here - the same keys are still offered, only without the
/// binding extension.
///
/// Gated to the feature of its one caller, the embedded reader's
/// `PubkeyAuthentication` arm: without `embedded-ssh` no reader consumes the
/// shape, and the fuzz workspace builds this crate without that feature under
/// `-D warnings`.
#[cfg(feature = "embedded-ssh")]
pub(in crate::ssh) fn parse_pubkey_auth_value(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "yes" | "true" | "unbound" | "host-bound" => Some(true),
        "no" | "false" => Some(false),
        _ => None,
    }
}

/// Parses a [`ValueKind::Time`] value into whole seconds.
///
/// A faithful port of upstream's `convtime` (openssh/misc.c:733-745) over
/// `convtime_double` (openssh/misc.c:657-726): a sequence of decimal
/// components, each with an optional case-insensitive qualifier - seconds
/// (bare or `s`), minutes (`m`), hours (`h`), days (`d`), weeks (`w`) -
/// summed together, so `1h30m` is 5400. A seconds component may appear
/// only once (openssh/misc.c:685-686); a fraction is allowed only on a
/// seconds component and must end in a digit (openssh/misc.c:710-717);
/// anything else - a negative, an empty string, a stray byte - is `None`.
/// Fractional seconds truncate and a total above `i32::MAX` is invalid,
/// both from `convtime` itself (openssh/misc.c:737-742).
#[cfg_attr(not(feature = "embedded-ssh"), allow(dead_code))]
pub(in crate::ssh) fn parse_time_value(value: &str) -> Option<u32> {
    const MINUTES: f64 = 60.0;
    const HOURS: f64 = 60.0 * MINUTES;
    const DAYS: f64 = 24.0 * HOURS;
    const WEEKS: f64 = 7.0 * DAYS;

    if value.is_empty() {
        return None;
    }
    let mut total = 0.0_f64;
    let mut seen_seconds = false;
    let mut rest = value;
    while !rest.is_empty() {
        // A component is a run of decimal digits and dots; any other lead
        // byte, and any form strtod would accept beyond plain decimals,
        // is rejected (openssh/misc.c:669-676).
        let span = rest
            .find(|c: char| !(c.is_ascii_digit() || c == '.'))
            .unwrap_or(rest.len());
        let number = &rest[..span];
        let val: f64 = number.parse().ok().filter(|v| *v >= 0.0)?;
        rest = &rest[span..];

        let multiplier = match rest.chars().next() {
            // Bare seconds and `s` share upstream's once-only flag
            // (openssh/misc.c:681-687).
            None | Some('s' | 'S') => {
                if seen_seconds {
                    return None;
                }
                seen_seconds = true;
                1.0
            }
            Some('m' | 'M') => MINUTES,
            Some('h' | 'H') => HOURS,
            Some('d' | 'D') => DAYS,
            Some('w' | 'W') => WEEKS,
            Some(_) => return None,
        };

        // A decimal point is legal only on a seconds component, and the
        // digits must continue past it (openssh/misc.c:710-717), so `1.`
        // and `1.5m` are both invalid.
        if number.contains('.')
            && (multiplier > 1.0 || !number.ends_with(|c: char| c.is_ascii_digit()))
        {
            return None;
        }

        total += val * multiplier;
        if !rest.is_empty() {
            rest = &rest[1..];
        }
    }
    if total > f64::from(i32::MAX) {
        return None;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Some(total as u32)
}

/// The address family a `AddressFamily` directive selects.
///
/// upstream: the three non-sentinel rows of `multistate_addressfamily`
/// (openssh/readconf.c:1038-1043), which map to `AF_INET`, `AF_INET6` and
/// `AF_UNSPEC`. Kept as a reader-neutral enum so the transport, not this
/// table, owns the mapping onto its own IP-preference knob.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
#[cfg_attr(not(feature = "embedded-ssh"), allow(dead_code))]
pub(in crate::ssh) enum AddressFamily {
    /// `any` - no preference (`AF_UNSPEC`).
    Any,
    /// `inet` - IPv4 only (`AF_INET`).
    Inet,
    /// `inet6` - IPv6 only (`AF_INET6`).
    Inet6,
}

impl AddressFamily {
    /// The lower-case spelling `ssh -G` dumps for this family
    /// (openssh/readconf.c:3582-3583 `fmt_multistate_int`). Consumed by the
    /// `ssh -G` differential harness, which is `#[cfg(test)]`.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(in crate::ssh) fn as_str(self) -> &'static str {
        match self {
            Self::Any => "any",
            Self::Inet => "inet",
            Self::Inet6 => "inet6",
        }
    }
}

/// Parses a [`ValueKind::AddressFamily`] value. Returns `None` for anything
/// outside the multistate set, which the caller reports as upstream's
/// `unsupported option "<arg>".` (openssh/readconf.c:1269).
///
/// upstream: `parse_multistate_value` compares with `strcasecmp`
/// (openssh/readconf.c:1109-1112), so the match is case-insensitive.
#[cfg_attr(not(feature = "embedded-ssh"), allow(dead_code))]
pub(in crate::ssh) fn parse_address_family(value: &str) -> Option<AddressFamily> {
    match value.trim().to_ascii_lowercase().as_str() {
        "any" => Some(AddressFamily::Any),
        "inet" => Some(AddressFamily::Inet),
        "inet6" => Some(AddressFamily::Inet6),
        _ => None,
    }
}

/// Parses a [`ValueKind::Int`] value the way upstream's `atoi_err` does:
/// `strtonum(arg, 0, INT_MAX)` (openssh/misc.c:2448-2456). `Err` carries
/// the full diagnostic upstream prints (`integer value <errstr>.`,
/// openssh/readconf.c:1579), so a caller maps it straight to a refusal.
///
/// The accepted range is `0..=i32::MAX`: a negative is `too small`, a value
/// past `i32::MAX` is `too large`, and a non-decimal token is `invalid`,
/// matching `strtonum`'s own `errstr` set.
#[cfg_attr(not(feature = "embedded-ssh"), allow(dead_code))]
pub(in crate::ssh) fn parse_int_value(value: &str) -> Result<u32, &'static str> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err("integer value missing.");
    }
    match trimmed.parse::<i64>() {
        Ok(v) if v < 0 => Err("integer value too small."),
        Ok(v) if v > i64::from(i32::MAX) => Err("integer value too large."),
        Ok(v) => Ok(v as u32),
        Err(_) => Err("integer value invalid."),
    }
}

/// Byte-level glob matcher for a single `Host` or `Match` pattern token:
/// `*` matches any run, `?` matches exactly one byte. No character
/// classes, no extended globs.
///
/// upstream: `match_pattern` (openssh/match.c:57-114). Case folding is the
/// caller's, because it differs per keyword - the `oHost` arm calls
/// `match_pattern` directly and folds nothing (openssh/readconf.c:1844),
/// while `Match host` routes through `match_hostname`, which lowercases
/// first (openssh/match.c:194-203).
pub(in crate::ssh) fn glob_matches(input: &[u8], glob: &[u8]) -> bool {
    if glob.is_empty() {
        return input.is_empty();
    }
    match glob[0] {
        b'*' => {
            if glob.len() == 1 {
                return true;
            }
            (0..=input.len()).any(|i| glob_matches(&input[i..], &glob[1..]))
        }
        b'?' => !input.is_empty() && glob_matches(&input[1..], &glob[1..]),
        c => !input.is_empty() && input[0] == c && glob_matches(&input[1..], &glob[1..]),
    }
}

/// How a pattern list matched a string.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(in crate::ssh) enum PatternListMatch {
    /// A `!`-negated subpattern matched, which excludes the string outright.
    Negative,
    /// No subpattern matched.
    None,
    /// A positive subpattern matched and no negated one did.
    Positive,
}

/// Matches `string` against a comma-separated pattern list - a port of
/// `match_pattern_list` (openssh/match.c:122-172).
///
/// Each subpattern may be `!`-negated; a matching negated subpattern
/// excludes the string outright, otherwise one positive match is needed.
/// With `dolower` the subpatterns are lowercased first.
pub(in crate::ssh) fn match_pattern_list(
    string: &str,
    list: &str,
    dolower: bool,
) -> PatternListMatch {
    // Upstream copies each subpattern into `char sub[1024]` and gives up
    // with "no match" on one that does not fit (openssh/match.c:147-149).
    const SUB_MAX: usize = 1023;
    // The scan ends at the list's end, so a trailing comma adds no empty
    // subpattern while a leading or doubled one does (:130-156).
    let body = list.strip_suffix(',').unwrap_or(list);
    if list.is_empty() {
        return PatternListMatch::None;
    }
    let mut positive = false;
    for sub in body.split(',') {
        let (negated, glob) = sub.strip_prefix('!').map_or((false, sub), |g| (true, g));
        if glob.len() >= SUB_MAX {
            return PatternListMatch::None;
        }
        let glob = if dolower {
            glob.to_ascii_lowercase()
        } else {
            glob.to_owned()
        };
        if glob_matches(string.as_bytes(), glob.as_bytes()) {
            if negated {
                return PatternListMatch::Negative;
            }
            positive = true;
        }
    }
    if positive {
        PatternListMatch::Positive
    } else {
        PatternListMatch::None
    }
}

/// Whether `keyword` (already lowercased) is excused by an `IgnoreUnknown`
/// list: upstream's `parse_token` returns `oIgnoredUnknownOption` only when
/// `match_pattern_list(cp, ignored_unknown, 1) == 1`
/// (openssh/readconf.c:977-979).
#[cfg_attr(not(feature = "embedded-ssh"), allow(dead_code))]
pub(in crate::ssh) fn ignore_unknown_matches(keyword: &str, list: &str) -> bool {
    match_pattern_list(keyword, list, true) == PatternListMatch::Positive
}

/// Why a scaled number did not parse: upstream's `errno`, rendered as its
/// `strerror` text.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(in crate::ssh) enum ScaledError {
    /// `EINVAL`: a stray character or unit.
    Invalid,
    /// `ERANGE`: too many digits, or overflow.
    Range,
}

impl std::fmt::Display for ScaledError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Invalid => "Invalid argument",
            #[cfg(target_os = "linux")]
            Self::Range => "Numerical result out of range",
            #[cfg(not(target_os = "linux"))]
            Self::Range => "Result too large",
        })
    }
}

/// Parses a number with an optional `B`/`K`/`M`/`G`/`T`/`P`/`E` binary
/// suffix and fraction (`1.5K` is 1536) - a port of `scan_scaled`
/// (openssh/openbsd-compat/fmt_scaled.c:76-214).
#[cfg_attr(not(feature = "embedded-ssh"), allow(dead_code))]
pub(in crate::ssh) fn parse_scaled_value(scaled: &str) -> Result<i64, ScaledError> {
    // `SCALE_LENGTH * 3` (openssh/openbsd-compat/fmt_scaled.c:70).
    const MAX_DIGITS: u32 = 21;
    const SCALE_CHARS: &[u8] = b"BKMGTPE";
    let bytes = scaled.trim_start().as_bytes();
    let mut i = 0;
    let mut sign = 0_i64;
    while i < bytes.len() && (bytes[i] == b'-' || bytes[i] == b'+') {
        if sign != 0 {
            return Err(ScaledError::Invalid);
        }
        sign = if bytes[i] == b'-' { -1 } else { 1 };
        i += 1;
    }
    let (mut whole, mut fpart) = (0_i64, 0_i64);
    let (mut ndigits, mut fract_digits) = (0_u32, 0_u32);
    while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
        let c = bytes[i];
        i += 1;
        if c == b'.' {
            if fract_digits > 0 {
                return Err(ScaledError::Invalid);
            }
            fract_digits = 1;
            continue;
        }
        let digit = i64::from(c - b'0');
        if fract_digits > 0 {
            // Extra fractional digits are ignored (:124-126).
            if fract_digits >= MAX_DIGITS - 1 {
                continue;
            }
            fract_digits += 1;
            fpart = fpart
                .checked_mul(10)
                .and_then(|f| f.checked_add(digit))
                .ok_or(ScaledError::Range)?;
        } else {
            ndigits += 1;
            if ndigits >= MAX_DIGITS {
                return Err(ScaledError::Range);
            }
            whole = whole
                .checked_mul(10)
                .and_then(|w| w.checked_add(digit))
                .ok_or(ScaledError::Range)?;
        }
    }
    if sign != 0 {
        whole *= sign;
    }
    // No unit: the fraction is discarded (:159-163).
    let Some(&unit) = bytes.get(i) else {
        return Ok(whole);
    };
    let Some(power) = SCALE_CHARS
        .iter()
        .position(|&c| c == unit.to_ascii_uppercase())
    else {
        return Err(ScaledError::Invalid);
    };
    if bytes.get(i + 1).is_some_and(u8::is_ascii_alphanumeric) {
        return Err(ScaledError::Invalid);
    }
    let scale = 1_i64 << (10 * power);
    whole = whole.checked_mul(scale).ok_or(ScaledError::Range)?;
    // Truncate the fraction so scaling cannot overflow, then scale it
    // (:189-201).
    while fpart >= i64::MAX / scale || fpart <= i64::MIN / scale {
        fpart /= 10;
        fract_digits = fract_digits.saturating_sub(1);
    }
    fpart *= scale;
    for _ in 1..fract_digits {
        fpart /= 10;
    }
    Ok(if sign == -1 {
        whole - fpart
    } else {
        whole + fpart
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The union both readers dispatch on, pinned by name.
    ///
    /// This is the anti-rename cell: the row exists so that dropping a
    /// keyword from [`KEYWORDS`] fails HERE as well as in whichever
    /// reader happens to consume it, and so that a keyword can never be
    /// declared in one reader's private set again.
    const UNION: &[(&str, Opcode)] = &[
        ("Host", Opcode::Host),
        ("Match", Opcode::Match),
        ("Include", Opcode::Include),
        ("Compression", Opcode::Compression),
        ("HostName", Opcode::Hostname),
        ("User", Opcode::User),
        ("Ciphers", Opcode::Ciphers),
        ("MACs", Opcode::Macs),
        ("KexAlgorithms", Opcode::KexAlgorithms),
        ("HostKeyAlgorithms", Opcode::HostKeyAlgorithms),
        ("RekeyLimit", Opcode::RekeyLimit),
        ("Port", Opcode::Port),
        ("IdentityFile", Opcode::IdentityFile),
        ("IdentitiesOnly", Opcode::IdentitiesOnly),
        ("IdentityAgent", Opcode::IdentityAgent),
        ("ConnectTimeout", Opcode::ConnectTimeout),
        ("AddressFamily", Opcode::AddressFamily),
        ("BindAddress", Opcode::BindAddress),
        ("BindInterface", Opcode::BindInterface),
        ("ConnectionAttempts", Opcode::ConnectionAttempts),
        ("IPQoS", Opcode::IPQoS),
        ("TCPKeepAlive", Opcode::TCPKeepAlive),
        // Obsolete alias, same opcode (openssh/readconf.c:253).
        ("KeepAlive", Opcode::TCPKeepAlive),
        ("ServerAliveInterval", Opcode::ServerAliveInterval),
        ("ServerAliveCountMax", Opcode::ServerAliveCountMax),
        ("ProxyCommand", Opcode::ProxyCommand),
        ("ProxyJump", Opcode::ProxyJump),
        ("ProxyUseFdpass", Opcode::ProxyUseFdpass),
        // The host-key verification family (openssh/readconf.c:237-243, :264, :290).
        ("UserKnownHostsFile", Opcode::UserKnownHostsFile),
        ("GlobalKnownHostsFile", Opcode::GlobalKnownHostsFile),
        ("HashKnownHosts", Opcode::HashKnownHosts),
        ("HostKeyAlias", Opcode::HostKeyAlias),
        ("CheckHostIP", Opcode::CheckHostIP),
        ("RevokedHostKeys", Opcode::RevokedHostKeys),
        // The authentication-control family (openssh/readconf.c:233-276, :328).
        ("PubkeyAuthentication", Opcode::PubkeyAuthentication),
        ("PasswordAuthentication", Opcode::PasswordAuthentication),
        (
            "KbdInteractiveAuthentication",
            Opcode::KbdInteractiveAuthentication,
        ),
        ("PreferredAuthentications", Opcode::PreferredAuthentications),
        ("BatchMode", Opcode::BatchMode),
        ("NumberOfPasswordPrompts", Opcode::NumberOfPasswordPrompts),
        ("CertificateFile", Opcode::CertificateFile),
        ("RequiredRSASize", Opcode::RequiredRSASize),
        // Aliases folding into the family opcodes above.
        (
            "ChallengeResponseAuthentication",
            Opcode::KbdInteractiveAuthentication,
        ),
        ("SKeyAuthentication", Opcode::KbdInteractiveAuthentication),
        ("TISAuthentication", Opcode::KbdInteractiveAuthentication),
        ("DSAAuthentication", Opcode::PubkeyAuthentication),
        // Obsolete alias, same opcode (openssh/readconf.c:245).
        ("IdentityFile2", Opcode::IdentityFile),
        ("IgnoreUnknown", Opcode::IgnoreUnknown),
    ];

    /// Every keyword upstream openssh-portable 10.0p1 recognises
    /// (openssh/readconf.c:192-337, 130 initialisers naming 126 spellings).
    /// Pinned here so a keyword dropped from [`KEYWORDS`] fails as a test
    /// rather than becoming a fatal `Bad configuration option` in a config
    /// `ssh` accepts.
    const UPSTREAM_10_0P1: &[&str] = &[
        "addkeystoagent",
        "addressfamily",
        "afstokenpassing",
        "batchmode",
        "bindaddress",
        "bindinterface",
        "canonicaldomains",
        "canonicalizefallbacklocal",
        "canonicalizehostname",
        "canonicalizemaxdots",
        "canonicalizepermittedcnames",
        "casignaturealgorithms",
        "certificatefile",
        "challengeresponseauthentication",
        "channeltimeout",
        "checkhostip",
        "cipher",
        "ciphers",
        "clearallforwardings",
        "compression",
        "compressionlevel",
        "connectionattempts",
        "connecttimeout",
        "controlmaster",
        "controlpath",
        "controlpersist",
        "dsaauthentication",
        "dynamicforward",
        "enableescapecommandline",
        "enablesshkeysign",
        "escapechar",
        "exitonforwardfailure",
        "fallbacktorsh",
        "fingerprinthash",
        "forkafterauthentication",
        "forwardagent",
        "forwardx11",
        "forwardx11timeout",
        "forwardx11trusted",
        "gatewayports",
        "globalknownhostsfile",
        "globalknownhostsfile2",
        "gssapiauthentication",
        "gssapidelegatecredentials",
        "hashknownhosts",
        "host",
        "hostbasedacceptedalgorithms",
        "hostbasedauthentication",
        "hostbasedkeytypes",
        "hostkeyalgorithms",
        "hostkeyalias",
        "hostname",
        "identitiesonly",
        "identityagent",
        "identityfile",
        "identityfile2",
        "ignoreunknown",
        "include",
        "ipqos",
        "kbdinteractiveauthentication",
        "kbdinteractivedevices",
        "keepalive",
        "kerberosauthentication",
        "kerberostgtpassing",
        "kexalgorithms",
        "knownhostscommand",
        "localcommand",
        "localforward",
        "loglevel",
        "logverbose",
        "macs",
        "match",
        "nohostauthenticationforlocalhost",
        "numberofpasswordprompts",
        "obscurekeystroketiming",
        "passwordauthentication",
        "permitlocalcommand",
        "permitremoteopen",
        "pkcs11provider",
        "port",
        "preferredauthentications",
        "protocol",
        "proxycommand",
        "proxyjump",
        "proxyusefdpass",
        "pubkeyacceptedalgorithms",
        "pubkeyacceptedkeytypes",
        "pubkeyauthentication",
        "rekeylimit",
        "remotecommand",
        "remoteforward",
        "requesttty",
        "requiredrsasize",
        "revokedhostkeys",
        "rhostsauthentication",
        "rhostsrsaauthentication",
        "rsaauthentication",
        "securitykeyprovider",
        "sendenv",
        "serveralivecountmax",
        "serveraliveinterval",
        "sessiontype",
        "setenv",
        "skeyauthentication",
        "smartcarddevice",
        "stdinnull",
        "streamlocalbindmask",
        "streamlocalbindunlink",
        "stricthostkeychecking",
        "syslogfacility",
        "tag",
        "tcpkeepalive",
        "tisauthentication",
        "tunnel",
        "tunneldevice",
        "updatehostkeys",
        "useprivilegedport",
        "user",
        "userknownhostsfile",
        "userknownhostsfile2",
        "useroaming",
        "usersh",
        "verifyhostkeydns",
        "versionaddendum",
        "visualhostkey",
        "xauthlocation",
    ];

    /// Whether the reader accepts `opcode` without acting on it.
    fn accepted_unread(opcode: Opcode) -> bool {
        matches!(
            opcode,
            Opcode::Ignore | Opcode::Deprecated | Opcode::Unsupported | Opcode::NotActedOn
        )
    }

    #[test]
    fn every_keyword_both_readers_use_resolves_from_the_one_table() {
        for (spelling, expected) in UNION {
            assert_eq!(parse_token(spelling), *expected, "keyword {spelling}");
        }
        let acted = KEYWORDS.iter().filter(|(_, op)| !accepted_unread(*op));
        assert_eq!(acted.count(), UNION.len(), "table and union disagree");
    }

    /// upstream lowercases the keyword before the table scan
    /// (openssh/readconf.c:1184), so every spelling resolves.
    #[test]
    fn keyword_lookup_is_case_insensitive() {
        for spelling in ["identityfile", "IDENTITYFILE", "IdEnTiTyFiLe"] {
            assert_eq!(parse_token(spelling), Opcode::IdentityFile);
        }
    }

    /// Non-vacuity for the two cells above: a keyword that is NOT in the
    /// table resolves to `Unknown`, so the lookup is a real table scan and
    /// not a function that answers every question affirmatively.
    #[test]
    fn a_keyword_outside_the_table_is_unknown() {
        for spelling in ["", "hostnamex", "hos", "forwardagents", "identityfile3"] {
            assert_eq!(parse_token(spelling), Opcode::Unknown, "keyword {spelling}");
        }
    }

    /// The table is upstream's, entire: every 10.0p1 spelling resolves,
    /// and nothing else is in it.
    #[test]
    fn the_table_is_exactly_upstreams_keyword_set() {
        for spelling in UPSTREAM_10_0P1 {
            assert_ne!(parse_token(spelling), Opcode::Unknown, "keyword {spelling}");
        }
        assert_eq!(KEYWORDS.len(), UPSTREAM_10_0P1.len());
    }

    /// upstream's accept-and-ignore categories keep their own opcodes,
    /// because each prints a different note (openssh/readconf.c:194-210).
    #[test]
    fn upstream_accept_and_ignore_categories_are_kept_apart() {
        assert_eq!(parse_token("protocol"), Opcode::Ignore);
        for spelling in [
            "cipher",
            "fallbacktorsh",
            "globalknownhostsfile2",
            "rhostsauthentication",
            "userknownhostsfile2",
            "useroaming",
            "usersh",
            "useprivilegedport",
        ] {
            assert_eq!(parse_token(spelling), Opcode::Deprecated, "{spelling}");
        }
        for spelling in [
            "afstokenpassing",
            "kerberosauthentication",
            "kerberostgtpassing",
            "rsaauthentication",
            "rhostsrsaauthentication",
            "compressionlevel",
        ] {
            assert_eq!(parse_token(spelling), Opcode::Unsupported, "{spelling}");
        }
    }

    /// The options a non-interactive single-channel transfer must never
    /// honour are accepted and NOT acted on. `RemoteCommand` above all:
    /// honouring it would replace `rsync --server`. The build-dependent
    /// GSSAPI and PKCS#11 keywords are accepted as a build with those
    /// features accepts them (openssh/readconf.c:213-226).
    #[test]
    fn options_the_transfer_must_not_honour_are_accepted_unread() {
        for spelling in [
            "remotecommand",
            "forwardagent",
            "localforward",
            "localcommand",
            "permitlocalcommand",
            "requesttty",
            "controlmaster",
            "gssapiauthentication",
            "gssapidelegatecredentials",
            "pkcs11provider",
            "smartcarddevice",
        ] {
            assert_eq!(parse_token(spelling), Opcode::NotActedOn, "{spelling}");
        }
    }

    /// `IgnoreUnknown` is `match_pattern_list(keyword, list, 1)`: comma
    /// separated, lowercased patterns, a matching negation excludes
    /// (openssh/match.c:122-172). Oracle: `IgnoreUnknown foo*,!foox` still
    /// refuses `foox` under `ssh -G`.
    #[test]
    fn ignore_unknown_follows_match_pattern_list() {
        assert!(ignore_unknown_matches("foobar", "foo*"));
        assert!(ignore_unknown_matches("foobar", "x,FOO*"));
        assert!(!ignore_unknown_matches("foox", "foo*,!foox"));
        assert!(!ignore_unknown_matches("foox", "!foox,foo*"));
        assert!(!ignore_unknown_matches("bar", "foo*"));
        assert!(!ignore_unknown_matches("bar", "!foo"));
    }

    /// `match_pattern_list`'s subpattern scan: a trailing comma adds no
    /// empty subpattern, a doubled one does, and a subpattern too long for
    /// upstream's 1024-byte buffer fails the whole list
    /// (openssh/match.c:122-172).
    #[test]
    fn match_pattern_list_follows_the_subpattern_scan() {
        use PatternListMatch::{Negative, None, Positive};
        assert_eq!(match_pattern_list("a", "", false), None);
        assert_eq!(match_pattern_list("", "", false), None);
        assert_eq!(match_pattern_list("", "a,", false), None);
        assert_eq!(match_pattern_list("", "a,,", false), Positive);
        assert_eq!(match_pattern_list("", ",a", false), Positive);
        assert_eq!(match_pattern_list("ab", "a?,!x", false), Positive);
        assert_eq!(match_pattern_list("ab", "a*,!ab", false), Negative);
        assert_eq!(match_pattern_list("AB", "a*", false), None);
        assert_eq!(match_pattern_list("ab", "A*", true), Positive);
        let long = "a".repeat(1023);
        assert_eq!(match_pattern_list(&long, &long, false), None);
        assert_eq!(match_pattern_list("a", &format!("a,{long}"), false), None);
        let fits = "a".repeat(1022);
        assert_eq!(match_pattern_list(&fits, &fits, false), Positive);
    }

    /// `scan_scaled`: binary units, fractions (kept only with a unit),
    /// one sign, and the `EINVAL`/`ERANGE` split
    /// (openssh/openbsd-compat/fmt_scaled.c:76-214). The values with a
    /// unit and a fraction are the ones `ssh -G` confirms in the
    /// differential.
    #[test]
    fn parse_scaled_value_mirrors_scan_scaled() {
        assert_eq!(parse_scaled_value("16"), Ok(16));
        assert_eq!(parse_scaled_value("1.9"), Ok(1));
        assert_eq!(parse_scaled_value("1.5K"), Ok(1536));
        assert_eq!(parse_scaled_value("0.5k"), Ok(512));
        assert_eq!(parse_scaled_value("1G"), Ok(1 << 30));
        assert_eq!(parse_scaled_value("2B"), Ok(2));
        assert_eq!(parse_scaled_value("7E"), Ok(7 << 60));
        assert_eq!(parse_scaled_value("K"), Ok(0));
        assert_eq!(parse_scaled_value("-1"), Ok(-1));
        assert_eq!(parse_scaled_value("+1K"), Ok(1024));
        assert_eq!(parse_scaled_value("-1.5K"), Ok(-1536));
        assert_eq!(parse_scaled_value("4x"), Err(ScaledError::Invalid));
        assert_eq!(parse_scaled_value("1Kb"), Err(ScaledError::Invalid));
        assert_eq!(parse_scaled_value("1.2.3"), Err(ScaledError::Invalid));
        assert_eq!(parse_scaled_value("+-1"), Err(ScaledError::Invalid));
        assert_eq!(parse_scaled_value("8E"), Err(ScaledError::Range));
        assert_eq!(parse_scaled_value(&"9".repeat(20)), Err(ScaledError::Range));
        assert_eq!(
            parse_scaled_value(&"1".repeat(19)),
            Ok(1_111_111_111_111_111_111)
        );
    }
    #[test]
    fn no_keyword_is_declared_twice() {
        for (i, (name, _)) in KEYWORDS.iter().enumerate() {
            assert!(
                !KEYWORDS[i + 1..].iter().any(|(other, _)| other == name),
                "keyword {name} declared twice"
            );
        }
    }

    /// The table's spellings must be the lower-case form upstream compares
    /// against, or `eq_ignore_ascii_case` would be hiding a typo.
    #[test]
    fn table_spellings_are_lowercase() {
        for (name, _) in KEYWORDS {
            assert_eq!(*name, name.to_ascii_lowercase(), "keyword {name}");
        }
    }

    /// The two capitalisations are upstream's, and they split by VALUE
    /// SHAPE rather than by keyword - the property that makes deriving the
    /// wording from [`ValueKind`] correct for a keyword not yet added.
    #[test]
    fn the_missing_argument_wording_splits_by_value_shape() {
        assert_eq!(
            Opcode::IdentitiesOnly.missing_argument(),
            Some("missing argument.")
        );
        assert_eq!(
            Opcode::Compression.missing_argument(),
            Some("missing argument.")
        );
        for opcode in [
            Opcode::Hostname,
            Opcode::User,
            Opcode::Port,
            Opcode::IdentityFile,
            Opcode::IdentityAgent,
        ] {
            assert_eq!(
                opcode.missing_argument(),
                Some("Missing argument."),
                "{opcode:?}"
            );
        }
        // The time arm has its own wording (openssh/readconf.c:1219-1220),
        // measured against real `ssh -G`. `ServerAliveInterval` shares the
        // arm, so it shares the wording.
        for opcode in [Opcode::ConnectTimeout, Opcode::ServerAliveInterval] {
            assert_eq!(
                opcode.missing_argument(),
                Some("missing time value."),
                "{opcode:?}"
            );
        }
        // The `parse_int` arm's absent value is `atoi_err(NULL) == "missing"`
        // (openssh/misc.c:2452), printed as `integer value missing.`.
        for opcode in [Opcode::ConnectionAttempts, Opcode::ServerAliveCountMax] {
            assert_eq!(
                opcode.missing_argument(),
                Some("integer value missing."),
                "{opcode:?}"
            );
        }
        // The address-family and keepalive multistates route through the
        // same `parse_multistate_value` as the plain flags, so they share
        // its `missing argument.` wording (openssh/readconf.c:1106).
        for opcode in [Opcode::AddressFamily, Opcode::TCPKeepAlive] {
            assert_eq!(
                opcode.missing_argument(),
                Some("missing argument."),
                "{opcode:?}"
            );
        }
        // The string arms print the capital-M `Missing argument.`.
        // `HostKeyAlias` and `RevokedHostKeys` route through `parse_string`
        // too (openssh/readconf.c:1497, :2444), so they share the wording.
        for opcode in [
            Opcode::BindAddress,
            Opcode::BindInterface,
            Opcode::HostKeyAlias,
            Opcode::RevokedHostKeys,
        ] {
            assert_eq!(
                opcode.missing_argument(),
                Some("Missing argument."),
                "{opcode:?}"
            );
        }
        // `HashKnownHosts` and `CheckHostIP` are `parse_flag` multistates,
        // so they share `missing argument.` (openssh/readconf.c:1106).
        for opcode in [Opcode::HashKnownHosts, Opcode::CheckHostIP] {
            assert_eq!(
                opcode.missing_argument(),
                Some("missing argument."),
                "{opcode:?}"
            );
        }
        // The known_hosts file lists are list-shaped, so an absent value is
        // not a fixed refusal (openssh/readconf.c:1600-1652).
        for opcode in [Opcode::UserKnownHostsFile, Opcode::GlobalKnownHostsFile] {
            assert_eq!(opcode.missing_argument(), None, "{opcode:?}");
        }
        // The block-opening keywords consume a LIST; an absent value
        // leaves the walk empty rather than refusing
        // (openssh/readconf.c:1829-1831). `Include` is list-shaped too:
        // its empty-argument refusal is the per-TOKEN empty check
        // (openssh/readconf.c:2081), not an absent-value one. `IPQoS`
        // refuses with a VALUE-interpolated `Bad IPQoS value: %s`, not a
        // fixed absent-value string, so it too has no entry.
        for opcode in [
            Opcode::Host,
            Opcode::Match,
            Opcode::Include,
            Opcode::IPQoS,
            Opcode::Unknown,
        ] {
            assert_eq!(opcode.missing_argument(), None, "{opcode:?}");
        }
    }

    /// `parse_int_value` mirrors `atoi_err`'s `strtonum(arg, 0, INT_MAX)`
    /// (openssh/misc.c:2448-2456): the accepted band and every `errstr`.
    #[test]
    fn int_values_cover_atoi_errs_range_and_errstrings() {
        assert_eq!(parse_int_value("0"), Ok(0));
        assert_eq!(parse_int_value("3"), Ok(3));
        assert_eq!(parse_int_value("2147483647"), Ok(i32::MAX as u32));
        assert_eq!(parse_int_value(""), Err("integer value missing."));
        assert_eq!(parse_int_value("-1"), Err("integer value too small."));
        assert_eq!(
            parse_int_value("2147483648"),
            Err("integer value too large.")
        );
        for bad in ["abc", "5x", "1.5", "0x10"] {
            assert_eq!(parse_int_value(bad), Err("integer value invalid."), "{bad}");
        }
    }

    /// `parse_address_family` is the `multistate_addressfamily` set
    /// (openssh/readconf.c:1038-1043), compared case-insensitively; a
    /// token outside it is `None` so the caller can refuse it.
    #[test]
    fn address_family_covers_the_multistate_set() {
        assert_eq!(parse_address_family("any"), Some(AddressFamily::Any));
        assert_eq!(parse_address_family("inet"), Some(AddressFamily::Inet));
        assert_eq!(parse_address_family("INET6"), Some(AddressFamily::Inet6));
        assert_eq!(AddressFamily::Any.as_str(), "any");
        assert_eq!(AddressFamily::Inet.as_str(), "inet");
        assert_eq!(AddressFamily::Inet6.as_str(), "inet6");
        for bad in ["", "ipv4", "yes", "unix"] {
            assert_eq!(parse_address_family(bad), None, "{bad}");
        }
    }

    /// The policy axis splits the one accumulating row from the scalar
    /// slots. `IdentityFile` appends on every active line
    /// (openssh/readconf.c:1394 `add_identity_file`); everything else
    /// is a first-obtained slot (openssh/readconf.c:1229).
    #[test]
    fn identityfile_accumulates_and_every_other_row_is_first_obtained() {
        assert_eq!(
            Opcode::IdentityFile.resolution_policy(),
            ResolutionPolicy::Accumulate
        );
        for opcode in [
            Opcode::Compression,
            Opcode::IdentitiesOnly,
            Opcode::Hostname,
            Opcode::User,
            Opcode::Port,
            Opcode::IdentityAgent,
            Opcode::ConnectTimeout,
            // The connection-establishment family: every one is a scalar
            // first-obtained slot upstream (`*intptr == -1` gate, or the
            // `charptr` set-once for the string arms, or
            // `ip_qos_interactive == -1` for IPQoS). None accumulate.
            Opcode::AddressFamily,
            Opcode::BindAddress,
            Opcode::BindInterface,
            Opcode::ConnectionAttempts,
            Opcode::IPQoS,
            Opcode::TCPKeepAlive,
            Opcode::ServerAliveInterval,
            Opcode::ServerAliveCountMax,
            // The host-key verification family: the flags and single-string
            // slots are scalar first-obtained upstream, and the known_hosts
            // file lists are first-obtained at LINE granularity - only the
            // first active line of a given keyword claims the list
            // (openssh/readconf.c:1603 `value = *uintptr == 0`), so a later
            // line adds nothing, exactly the FirstObtained cross-line rule.
            Opcode::UserKnownHostsFile,
            Opcode::GlobalKnownHostsFile,
            Opcode::HashKnownHosts,
            Opcode::HostKeyAlias,
            Opcode::CheckHostIP,
            Opcode::RevokedHostKeys,
        ] {
            assert_eq!(
                opcode.resolution_policy(),
                ResolutionPolicy::FirstObtained,
                "{opcode:?}"
            );
        }
    }

    #[test]
    fn may_assign_gates_only_the_first_obtained_claim() {
        assert!(ResolutionPolicy::FirstObtained.may_assign(false));
        assert!(!ResolutionPolicy::FirstObtained.may_assign(true));
        assert!(ResolutionPolicy::Accumulate.may_assign(false));
        assert!(ResolutionPolicy::Accumulate.may_assign(true));
    }

    /// Every alias spelling readconf.c accepts in the auth-control family
    /// resolves to the same opcode as its canonical keyword, matched
    /// case-insensitively (openssh/readconf.c:236-240).
    #[test]
    fn auth_family_aliases_fold_into_their_canonical_opcode() {
        for spelling in [
            "challengeresponseauthentication",
            "ChallengeResponseAuthentication",
            "skeyauthentication",
            "tisauthentication",
        ] {
            assert_eq!(
                parse_token(spelling),
                Opcode::KbdInteractiveAuthentication,
                "{spelling}"
            );
        }
        for spelling in ["dsaauthentication", "DSAAuthentication"] {
            assert_eq!(
                parse_token(spelling),
                Opcode::PubkeyAuthentication,
                "{spelling}"
            );
        }
    }

    /// The auth-control family's value shapes drive the right missing-value
    /// wording: the flags and the pubkey multistate print `missing argument.`,
    /// the two int keywords print `integer value missing.`, and the
    /// string-shaped `CertificateFile`/`PreferredAuthentications` print
    /// `Missing argument.` (openssh/readconf.c:1195, :1372, :1398, :1579).
    #[test]
    fn auth_family_missing_argument_wording_matches_value_shape() {
        for opcode in [
            Opcode::PasswordAuthentication,
            Opcode::KbdInteractiveAuthentication,
            Opcode::BatchMode,
            Opcode::PubkeyAuthentication,
        ] {
            assert_eq!(
                opcode.missing_argument(),
                Some("missing argument."),
                "{opcode:?}"
            );
        }
        for opcode in [Opcode::NumberOfPasswordPrompts, Opcode::RequiredRSASize] {
            assert_eq!(
                opcode.missing_argument(),
                Some("integer value missing."),
                "{opcode:?}"
            );
        }
        for opcode in [Opcode::CertificateFile, Opcode::PreferredAuthentications] {
            assert_eq!(
                opcode.missing_argument(),
                Some("Missing argument."),
                "{opcode:?}"
            );
        }
    }

    /// `CertificateFile` is the family's one accumulating row - it appends on
    /// every active line (openssh/readconf.c:1385 `add_certificate_file`) -
    /// while every other family keyword is a first-obtained scalar slot.
    #[test]
    fn certificatefile_accumulates_other_auth_family_rows_are_first_obtained() {
        assert_eq!(
            Opcode::CertificateFile.resolution_policy(),
            ResolutionPolicy::Accumulate
        );
        for opcode in [
            Opcode::PubkeyAuthentication,
            Opcode::PasswordAuthentication,
            Opcode::KbdInteractiveAuthentication,
            Opcode::PreferredAuthentications,
            Opcode::BatchMode,
            Opcode::NumberOfPasswordPrompts,
            Opcode::RequiredRSASize,
        ] {
            assert_eq!(
                opcode.resolution_policy(),
                ResolutionPolicy::FirstObtained,
                "{opcode:?}"
            );
        }
    }

    /// `parse_pubkey_auth_value` is the `multistate_pubkey_auth` set
    /// (openssh/readconf.c:1086-1094): the two bound-key selectors collapse
    /// onto "enabled", and a token outside the set is `None` so the caller
    /// can refuse it with the spelling. Gated with the helper, which exists
    /// only for the embedded reader.
    #[cfg(feature = "embedded-ssh")]
    #[test]
    fn pubkey_auth_covers_the_multistate_set() {
        for on in ["yes", "YES", "true", "unbound", "host-bound", "HOST-BOUND"] {
            assert_eq!(parse_pubkey_auth_value(on), Some(true), "{on}");
        }
        for off in ["no", "NO", "false"] {
            assert_eq!(parse_pubkey_auth_value(off), Some(false), "{off}");
        }
        for bad in ["", "maybe", "1", "ask"] {
            assert_eq!(parse_pubkey_auth_value(bad), None, "{bad}");
        }
    }

    /// `convtime` accepts the documented decimal-with-qualifier forms.
    /// Every accepted row was measured against real `ssh -G`
    /// (`ConnectTimeout <value>` in a fixture), not derived from the C.
    #[test]
    fn time_values_cover_convtimes_accepted_forms() {
        for (input, expected) in [
            ("0", 0),
            ("7", 7),
            ("30S", 30),
            ("90m", 5400),
            ("1m30s", 90),
            ("1h30m", 5400),
            ("2d", 172_800),
            ("1w", 604_800),
            ("1.5", 1),
            ("2147483647", i32::MAX as u32),
        ] {
            assert_eq!(parse_time_value(input), Some(expected), "{input}");
        }
    }

    /// The refusals: negatives (first byte fails the digit test,
    /// openssh/misc.c:669-670), a repeated seconds component
    /// (openssh/misc.c:685-686), fractions outside seconds and a trailing
    /// dot (openssh/misc.c:710-717), overflow past `INT_MAX`
    /// (openssh/misc.c:740-741), and `none`, which is NOT convtime's to
    /// accept - the readconf arm strcmp's it case-SENSITIVELY before
    /// calling convtime (openssh/readconf.c:1222).
    #[test]
    fn time_values_reject_what_convtime_rejects() {
        for input in [
            "",
            "-5",
            "bogus",
            "none",
            "NONE",
            "5x",
            "1.",
            ".",
            "1.5m",
            "5s5",
            "5 s",
            "2147483648",
            "1e3",
            "+5",
        ] {
            assert_eq!(parse_time_value(input), None, "{input}");
        }
    }

    #[test]
    fn split_directive_accepts_both_separators() {
        assert_eq!(split_directive("Port 2222"), Some(("Port", "2222")));
        assert_eq!(split_directive("Port=2222"), Some(("Port", "2222")));
        assert_eq!(split_directive("Port = 2222"), Some(("Port", "2222")));
        assert_eq!(split_directive("Port"), None);
        assert_eq!(split_directive("Port "), None);
    }

    #[test]
    fn flag_values_cover_upstreams_multistate_set() {
        for yes in ["yes", "YES", "true", "True"] {
            assert_eq!(parse_flag_value(yes), Some(true), "{yes}");
        }
        for no in ["no", "NO", "false", "False"] {
            assert_eq!(parse_flag_value(no), Some(false), "{no}");
        }
        for other in ["", "1", "on", "ask"] {
            assert_eq!(parse_flag_value(other), None, "{other}");
        }
    }

    #[test]
    fn glob_star_and_question_mark() {
        assert!(glob_matches(b"anything", b"*"));
        assert!(glob_matches(b"", b"*"));
        assert!(glob_matches(b"a.example.com", b"*.example.com"));
        assert!(!glob_matches(b"a.example.org", b"*.example.com"));
        assert!(glob_matches(b"a", b"?"));
        assert!(!glob_matches(b"ab", b"?"));
        assert!(glob_matches(b"ab", b"??"));
        assert!(glob_matches(b"exact", b"exact"));
        assert!(!glob_matches(b"exact", b"exac"));
        // Case folding is the CALLER's: the matcher itself never folds
        // (openssh/match.c:105-106).
        assert!(!glob_matches(b"web1", b"WEB1"));
    }
}

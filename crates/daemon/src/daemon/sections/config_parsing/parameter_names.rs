// The daemon parameter names a config line or `--dparam` may use.

/// Every daemon parameter name, whitespace-folded and lowercased the way
/// `normalize_param_name` folds a config key.
///
/// upstream: daemon-parm.txt - the `Globals:` and `Locals:` blocks generate
/// loadparm.c's `parm_table`, whose labels are the public names with `_`
/// rendered as a space (daemon-parm.awk). The oc-only names follow them; see
/// docs/oc-extension-env-reference.md.
const PARAMETER_NAMES: &[&str] = &[
    // Globals
    "address",
    "daemonchroot",
    "daemongid",
    "daemonuid",
    "motdfile",
    "pidfile",
    "proxyprotocolhosts",
    "socketoptions",
    "listenbacklog",
    "port",
    "proxyprotocol",
    // Locals
    "authdigest",
    "authusers",
    "charset",
    "comment",
    "dontcompress",
    "earlyexec",
    "exclude",
    "excludefrom",
    "filter",
    "gid",
    "hostsallow",
    "hostsdeny",
    "include",
    "includefrom",
    "incomingchmod",
    "lockfile",
    "logfile",
    "logformat",
    "name",
    "nameconverter",
    "outgoingchmod",
    "post-xferexec",
    "pre-xferexec",
    "refuseoptions",
    "secretsfile",
    "syslogtag",
    "uid",
    "path",
    "tempdir",
    "maxconnections",
    "maxverbosity",
    "timeout",
    "syslogfacility",
    "fakesuper",
    "forwardlookup",
    "ignoreerrors",
    "ignorenonreadable",
    "insecurelinks",
    "list",
    "readonly",
    "reverselookup",
    "strictmodes",
    "transferlogging",
    "writeonly",
    "mungesymlinks",
    "numericids",
    "opennoatime",
    "usechroot",
    // oc-only
    "bwlimit",
    "motd",
    "acceptorthreads",
    "rsyncport",
    "incoming-chmod",
    "outgoing-chmod",
    #[cfg(feature = "quic")]
    "quiccertfile",
    #[cfg(feature = "quic")]
    "quickeyfile",
    #[cfg(feature = "quic")]
    "quicclientcafile",
    #[cfg(feature = "quic")]
    "quicport",
];

/// Reports whether `name` is a daemon parameter, compared the way loadparm.c's
/// map_parameter() compares labels (strwiEQ: case- and whitespace-insensitive).
pub fn is_daemon_parameter(name: &str) -> bool {
    PARAMETER_NAMES.contains(&normalize_param_name(name).as_str())
}

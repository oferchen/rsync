// Global-section P_LOCAL parameter defaults.
//
// Holds the default values for per-module (P_LOCAL) parameters that appear in
// the global section and are inherited by every module that does not override
// them.

/// Default values for P_LOCAL module parameters set in the global section.
///
/// upstream: loadparm.c - when a P_LOCAL parameter appears in the global
/// section, it sets the default value (`def_ptr`) that all subsequently
/// parsed modules inherit via `init_section()` / `copy_section()`.
#[derive(Clone, Default)]
struct GlobalModuleDefaults {
    exclude: Vec<String>,
    include: Vec<String>,
    filter: Vec<String>,
    max_verbosity: Option<i32>,
    transfer_logging: Option<bool>,
    log_format: Option<String>,
    log_file: Option<PathBuf>,
    hosts_allow: Option<Vec<HostPattern>>,
    hosts_deny: Option<Vec<HostPattern>>,
    timeout: Option<Option<NonZeroU64>>,
    dont_compress: Option<String>,
    read_only: Option<bool>,
    write_only: Option<bool>,
    listable: Option<bool>,
    munge_symlinks: Option<Option<bool>>,
    numeric_ids: Option<bool>,
    fake_super: Option<bool>,
    insecure_links: Option<bool>,
    max_connections: Option<MaxConnections>,
    ignore_errors: Option<bool>,
    ignore_nonreadable: Option<bool>,
    strict_modes: Option<bool>,
    forward_lookup: Option<bool>,
    reverse_lookup: Option<bool>,
    syslog_tag: Option<String>,
    syslog_facility: Option<String>,
    open_noatime: Option<bool>,
    exclude_from: Option<PathBuf>,
    include_from: Option<PathBuf>,
    comment: Option<String>,
    early_exec: Option<String>,
    pre_xfer_exec: Option<String>,
    post_xfer_exec: Option<String>,
    name_converter: Option<String>,
    temp_dir: Option<String>,
    charset: Option<String>,
    // upstream: daemon-parm.txt `Locals:` - `uid`/`gid` are P_LOCAL. A value in
    // the global section is the default `lp_uid(i)`/`lp_gid(i)` every module
    // inherits (clientserver.c:781,790 read the per-module value). These are
    // distinct from the P_GLOBAL `daemon uid`/`daemon gid` process-wide drop
    // (clientserver.c:1363,1376 `lp_daemon_gid`/`lp_daemon_uid`).
    uid: Option<u32>,
    gid: Option<GidSetting>,
    // upstream: daemon-parm.h:262 marks `auth users` P_LOCAL, so a global-section
    // `auth users` becomes every module's default via loadparm.c
    // init_section()/copy_section(). authenticate.c:228 auth_server() then reads
    // lp_auth_users(module) and requires authentication whenever it is non-empty,
    // so a module inheriting the default authenticates like one with its own list.
    auth_users: Option<Vec<AuthUser>>,
    // upstream: daemon-parm.h:273 marks `auth digest` P_LOCAL, so a global
    // value is every module's default by the same init_section()/copy_section()
    // route as `auth users` above.
    auth_digest: Option<String>,
}

impl GlobalModuleDefaults {
    /// Builds the P_LOCAL defaults a module section finalizes against, given
    /// the globals in force when its `[name]` header was read (`snapshot`) and
    /// the globals in force once the whole config has been parsed (`latest`).
    ///
    /// upstream: loadparm.c:347-348 - `FN_LOCAL_STRING(fn, val)` expands to
    /// `if (LP_SNUM_OK(i) && iSECTION(i).val) RETURN_EXPANDED(iSECTION(i).val)
    /// else RETURN_EXPANDED(Vars.l.val)`, and clientserver.c:781-783 calls
    /// `lp_uid(i)` only when a client selects the module - long after
    /// `lp_load()` finished. A string-typed P_LOCAL parameter therefore
    /// resolves its default at ACCESS time, so a global set *after* a section
    /// (or after the `&include`/`&merge` that declared it) still applies to
    /// that section. `FN_LOCAL_BOOL`/`FN_LOCAL_INTEGER` (loadparm.c:351-356)
    /// carry no such fallback: they read `iSECTION(i).val`, which
    /// `init_section()` filled from `Vars.l` when the section was created, so
    /// those keep creation-time semantics.
    fn resolve(snapshot: &Self, latest: &Self) -> Self {
        Self {
            // Access-time (FN_LOCAL_STRING / FN_LOCAL_STRING_SHELL).
            exclude: latest.exclude.clone(),
            include: latest.include.clone(),
            filter: latest.filter.clone(),
            log_format: latest.log_format.clone(),
            log_file: latest.log_file.clone(),
            hosts_allow: latest.hosts_allow.clone(),
            hosts_deny: latest.hosts_deny.clone(),
            dont_compress: latest.dont_compress.clone(),
            syslog_tag: latest.syslog_tag.clone(),
            exclude_from: latest.exclude_from.clone(),
            include_from: latest.include_from.clone(),
            comment: latest.comment.clone(),
            early_exec: latest.early_exec.clone(),
            pre_xfer_exec: latest.pre_xfer_exec.clone(),
            post_xfer_exec: latest.post_xfer_exec.clone(),
            name_converter: latest.name_converter.clone(),
            temp_dir: latest.temp_dir.clone(),
            charset: latest.charset.clone(),
            uid: latest.uid,
            gid: latest.gid.clone(),
            auth_users: latest.auth_users.clone(),
            auth_digest: latest.auth_digest.clone(),
            // Creation-time (FN_LOCAL_BOOL / FN_LOCAL_INTEGER).
            max_verbosity: snapshot.max_verbosity,
            transfer_logging: snapshot.transfer_logging,
            timeout: snapshot.timeout,
            read_only: snapshot.read_only,
            write_only: snapshot.write_only,
            listable: snapshot.listable,
            munge_symlinks: snapshot.munge_symlinks,
            numeric_ids: snapshot.numeric_ids,
            fake_super: snapshot.fake_super,
            insecure_links: snapshot.insecure_links,
            max_connections: snapshot.max_connections,
            ignore_errors: snapshot.ignore_errors,
            ignore_nonreadable: snapshot.ignore_nonreadable,
            strict_modes: snapshot.strict_modes,
            forward_lookup: snapshot.forward_lookup,
            reverse_lookup: snapshot.reverse_lookup,
            syslog_facility: snapshot.syslog_facility.clone(),
            open_noatime: snapshot.open_noatime,
        }
    }
}

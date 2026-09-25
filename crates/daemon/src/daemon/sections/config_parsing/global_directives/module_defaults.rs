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
    /// upstream: loadparm.c:394-398 init_section() copies `Vars.l` into the new
    /// section, and loadparm.c:347-348 `FN_LOCAL_STRING(fn, val)` expands to
    /// `if (LP_SNUM_OK(i) && iSECTION(i).val) RETURN_EXPANDED(iSECTION(i).val)
    /// else RETURN_EXPANDED(Vars.l.val)`. clientserver.c:781-783 calls it only
    /// when a client selects the module - long after `lp_load()` finished. So
    /// a string-typed P_LOCAL parameter whose default is NULL takes the copied
    /// value when there is one, and the final global value only when there is
    /// not. A string whose built-in default is non-NULL (`dont compress`,
    /// `log format`, `syslog tag`) always has a copied value, as do the
    /// `FN_LOCAL_BOOL`/`FN_LOCAL_INTEGER` ones (loadparm.c:351-356): those keep
    /// creation-time semantics.
    fn resolve(snapshot: &Self, latest: &Self) -> Self {
        fn copied_or_latest<T: Clone>(copied: &Option<T>, latest: &Option<T>) -> Option<T> {
            copied.as_ref().or(latest.as_ref()).cloned()
        }
        fn copied_or_latest_list(copied: &[String], latest: &[String]) -> Vec<String> {
            if copied.is_empty() { latest } else { copied }.to_vec()
        }
        Self {
            // FN_LOCAL_STRING with a NULL default.
            exclude: copied_or_latest_list(&snapshot.exclude, &latest.exclude),
            include: copied_or_latest_list(&snapshot.include, &latest.include),
            filter: copied_or_latest_list(&snapshot.filter, &latest.filter),
            log_file: copied_or_latest(&snapshot.log_file, &latest.log_file),
            hosts_allow: copied_or_latest(&snapshot.hosts_allow, &latest.hosts_allow),
            hosts_deny: copied_or_latest(&snapshot.hosts_deny, &latest.hosts_deny),
            exclude_from: copied_or_latest(&snapshot.exclude_from, &latest.exclude_from),
            include_from: copied_or_latest(&snapshot.include_from, &latest.include_from),
            comment: copied_or_latest(&snapshot.comment, &latest.comment),
            early_exec: copied_or_latest(&snapshot.early_exec, &latest.early_exec),
            pre_xfer_exec: copied_or_latest(&snapshot.pre_xfer_exec, &latest.pre_xfer_exec),
            post_xfer_exec: copied_or_latest(&snapshot.post_xfer_exec, &latest.post_xfer_exec),
            name_converter: copied_or_latest(&snapshot.name_converter, &latest.name_converter),
            temp_dir: copied_or_latest(&snapshot.temp_dir, &latest.temp_dir),
            charset: copied_or_latest(&snapshot.charset, &latest.charset),
            uid: copied_or_latest(&snapshot.uid, &latest.uid),
            gid: copied_or_latest(&snapshot.gid, &latest.gid),
            auth_users: copied_or_latest(&snapshot.auth_users, &latest.auth_users),
            auth_digest: copied_or_latest(&snapshot.auth_digest, &latest.auth_digest),
            // FN_LOCAL_STRING with a non-NULL built-in default.
            log_format: snapshot.log_format.clone(),
            dont_compress: snapshot.dont_compress.clone(),
            syslog_tag: snapshot.syslog_tag.clone(),
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

/// Builder for constructing validated `ModuleDefinition` instances.
///
/// Accumulates per-module directives from `rsyncd.conf` and produces a
/// `ModuleDefinition` via [`finish`](Self::finish). Each setter enforces
/// duplicate-detection so the same directive cannot appear twice within a
/// single module section.
///
/// upstream: loadparm.c - per-module parameter accumulation.
struct ModuleDefinitionBuilder {
    name: String,
    path: Option<PathBuf>,
    comment: Option<String>,
    hosts_allow: Option<Vec<HostPattern>>,
    hosts_deny: Option<Vec<HostPattern>>,
    auth_users: Option<Vec<AuthUser>>,
    secrets_file: Option<PathBuf>,
    declaration_line: usize,
    bandwidth_limit: Option<NonZeroU64>,
    bandwidth_limit_specified: bool,
    bandwidth_burst: Option<NonZeroU64>,
    bandwidth_burst_specified: bool,
    bandwidth_limit_set: bool,
    refuse_options: Option<Vec<String>>,
    read_only: Option<bool>,
    write_only: Option<bool>,
    numeric_ids: Option<bool>,
    uid: Option<u32>,
    gid: Option<u32>,
    timeout: Option<Option<NonZeroU64>>,
    listable: Option<bool>,
    use_chroot: Option<bool>,
    max_connections: Option<Option<NonZeroU32>>,
    incoming_chmod: Option<Option<String>>,
    outgoing_chmod: Option<Option<String>>,
    fake_super: Option<bool>,
    munge_symlinks: Option<Option<bool>>,
    max_verbosity: Option<i32>,
    ignore_errors: Option<bool>,
    ignore_nonreadable: Option<bool>,
    transfer_logging: Option<bool>,
    log_format: Option<Option<String>>,
    dont_compress: Option<Option<String>>,
    early_exec: Option<Option<String>>,
    pre_xfer_exec: Option<Option<String>>,
    post_xfer_exec: Option<Option<String>>,
    name_converter: Option<Option<String>>,
    temp_dir: Option<Option<String>>,
    charset: Option<Option<String>>,
    forward_lookup: Option<bool>,
    strict_modes: Option<bool>,
    exclude_from: Option<PathBuf>,
    include_from: Option<PathBuf>,
    open_noatime: Option<bool>,
    log_file: Option<PathBuf>,
    /// Direct filter rules for this module.
    ///
    /// upstream: daemon-parm.h - `filter` STRING, P_LOCAL.
    filter: Vec<String>,
    /// Direct exclude rules for this module.
    ///
    /// upstream: daemon-parm.h - `exclude` STRING, P_LOCAL.
    exclude: Vec<String>,
    /// Direct include rules for this module.
    ///
    /// upstream: daemon-parm.h - `include` STRING, P_LOCAL.
    include: Vec<String>,
}

impl ModuleDefinitionBuilder {
    const fn new(name: String, line: usize) -> Self {
        Self {
            name,
            path: None,
            comment: None,
            hosts_allow: None,
            hosts_deny: None,
            auth_users: None,
            secrets_file: None,
            declaration_line: line,
            bandwidth_limit: None,
            bandwidth_limit_specified: false,
            bandwidth_burst: None,
            bandwidth_burst_specified: false,
            bandwidth_limit_set: false,
            refuse_options: None,
            read_only: None,
            write_only: None,
            numeric_ids: None,
            uid: None,
            gid: None,
            timeout: None,
            listable: None,
            use_chroot: None,
            max_connections: None,
            incoming_chmod: None,
            outgoing_chmod: None,
            fake_super: None,
            munge_symlinks: None,
            max_verbosity: None,
            ignore_errors: None,
            ignore_nonreadable: None,
            transfer_logging: None,
            log_format: None,
            dont_compress: None,
            early_exec: None,
            pre_xfer_exec: None,
            post_xfer_exec: None,
            name_converter: None,
            temp_dir: None,
            charset: None,
            forward_lookup: None,
            strict_modes: None,
            exclude_from: None,
            include_from: None,
            open_noatime: None,
            log_file: None,
            filter: Vec::new(),
            exclude: Vec::new(),
            include: Vec::new(),
        }
    }
}

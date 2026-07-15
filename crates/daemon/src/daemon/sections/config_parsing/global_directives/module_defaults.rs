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
    max_connections: Option<Option<NonZeroU32>>,
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
}

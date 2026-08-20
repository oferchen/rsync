fn log_connection(log: &SharedLogSink, host: &str, peer_addr: SocketAddr) {
    let ip = peer_addr.ip();
    let text = format!("connect from {host} ({ip})");
    let message = rsync_info!(text).with_role(Role::Daemon);
    log_message(log, &message);
}

pub(crate) fn log_list_request(log: &SharedLogSink, host: &str, peer_addr: SocketAddr) {
    let ip = peer_addr.ip();
    // upstream: clientserver.c:1555 - `module-list request from %s (%s)`
    let text = format!("module-list request from {host} ({ip})");
    let message = rsync_info!(text).with_role(Role::Daemon);
    log_message(log, &message);
}

pub(crate) fn log_module_request(
    log: &SharedLogSink,
    host: &str,
    peer_ip: IpAddr,
    module: &str,
) {
    let module_display = sanitize_module_identifier(module);
    // upstream: clientserver.c:787 - `rsync allowed access on module %s from %s (%s)`
    let text = format!("rsync allowed access on module {module_display} from {host} ({peer_ip})");
    let message = rsync_info!(text).with_role(Role::Daemon);
    log_message(log, &message);
}

/// Emits a structured warning when a module rejects a connection because
/// its per-module `max connections` cap has been reached.
///
/// Mirrors the global cap warning emitted from
/// [`log_max_connections_rejection`] so operators see one consistent
/// shape across both admission paths. Fields are stable and named:
/// `which` carries the module name (sanitised to strip control chars),
/// `peer` records the rejected client IP, `cap` is the configured limit
/// that triggered the refusal (negative when the directive disables the
/// module), and `current` is the active connection count observed at the
/// refusal moment.
pub(crate) fn log_module_limit(
    log: &SharedLogSink,
    host: &str,
    peer_ip: IpAddr,
    module: &str,
    limit: i32,
    current: u32,
) {
    let module_display = sanitize_module_identifier(module);
    let text = format!(
        "max-connections cap reached: which={module_display} peer={host} ({peer_ip}) cap={limit} current={current}"
    );
    let message = rsync_warning!(text).with_role(Role::Daemon);
    log_message(log, &message);
}

fn log_module_lock_error(
    log: &SharedLogSink,
    host: &str,
    peer_ip: IpAddr,
    module: &str,
    error: &io::Error,
) {
    let module_display = sanitize_module_identifier(module);
    let text = format!(
        "failed to update lock for module '{module_display}' requested from {host} ({peer_ip}): {error}"
    );
    let message = rsync_error!(FEATURE_UNAVAILABLE_EXIT_CODE, text).with_role(Role::Daemon);
    log_message(log, &message);
}

fn log_module_refused_option(
    log: &SharedLogSink,
    host: &str,
    peer_ip: IpAddr,
    module: &str,
    refused: &str,
) {
    let module_display = sanitize_module_identifier(module);
    let text = format!(
        "refusing option '{refused}' for module '{module_display}' from {host} ({peer_ip})"
    );
    let message = rsync_info!(text).with_role(Role::Daemon);
    log_message(log, &message);
}

pub(crate) fn log_module_auth_failure(
    log: &SharedLogSink,
    host: &str,
    peer_ip: IpAddr,
    module: &str,
    suffix: Option<&str>,
) {
    let module_display = sanitize_module_identifier(module);
    // upstream: `auth failed on module %s from %s (%s)` is the common prefix;
    // what follows differs by refusal. authenticate.c:318 / :325 append `: %s`
    // for the digest-floor arms, and :433 appends ` for %s: %s` once a username
    // is known. `AuthDenial::log_suffix` picks between those shapes, so this
    // layer just concatenates - one owner for the prefix, one for the tail.
    let text = match suffix {
        Some(suffix) => {
            format!("auth failed on module {module_display} from {host} ({peer_ip}){suffix}")
        }
        None => format!("auth failed on module {module_display} from {host} ({peer_ip})"),
    };
    let message = rsync_info!(text).with_role(Role::Daemon);
    log_message(log, &message);
}

pub(crate) fn log_module_denied(
    log: &SharedLogSink,
    host: &str,
    peer_ip: IpAddr,
    module: &str,
) {
    let module_display = sanitize_module_identifier(module);
    // upstream: clientserver.c:774 - `rsync denied on module %s from %s (%s)`
    let text = format!("rsync denied on module {module_display} from {host} ({peer_ip})");
    let message = rsync_info!(text).with_role(Role::Daemon);
    log_message(log, &message);
}

pub(crate) fn log_unknown_module(
    log: &SharedLogSink,
    host: &str,
    peer_ip: IpAddr,
    module: &str,
) {
    let module_display = sanitize_module_identifier(module);
    // upstream: clientserver.c:1568 - `unknown module '%s' tried from %s (%s)`
    let text = format!("unknown module '{module_display}' tried from {host} ({peer_ip})");
    let message = rsync_info!(text).with_role(Role::Daemon);
    log_message(log, &message);
}


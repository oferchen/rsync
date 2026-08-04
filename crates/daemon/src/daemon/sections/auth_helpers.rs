fn log_connection(log: &SharedLogSink, host: Option<&str>, peer_addr: SocketAddr) {
    let display = format_host(host, peer_addr.ip());
    let ip = peer_addr.ip();
    let text = format!("connect from {display} ({ip})");
    let message = rsync_info!(text).with_role(Role::Daemon);
    log_message(log, &message);
}

pub(crate) fn log_list_request(log: &SharedLogSink, host: Option<&str>, peer_addr: SocketAddr) {
    let display = format_host(host, peer_addr.ip());
    let ip = peer_addr.ip();
    // upstream: clientserver.c:1421 - `module-list request from %s (%s)`
    let text = format!("module-list request from {display} ({ip})");
    let message = rsync_info!(text).with_role(Role::Daemon);
    log_message(log, &message);
}

pub(crate) fn log_module_request(
    log: &SharedLogSink,
    host: Option<&str>,
    peer_ip: IpAddr,
    module: &str,
) {
    let display = format_host(host, peer_ip);
    let module_display = sanitize_module_identifier(module);
    // upstream: clientserver.c:742 - `rsync allowed access on module %s from %s (%s)`
    let text = format!("rsync allowed access on module {module_display} from {display} ({peer_ip})");
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
    host: Option<&str>,
    peer_ip: IpAddr,
    module: &str,
    limit: i32,
    current: u32,
) {
    let display = format_host(host, peer_ip);
    let module_display = sanitize_module_identifier(module);
    let text = format!(
        "max-connections cap reached: which={module_display} peer={display} ({peer_ip}) cap={limit} current={current}"
    );
    let message = rsync_warning!(text).with_role(Role::Daemon);
    log_message(log, &message);
}

fn log_module_lock_error(
    log: &SharedLogSink,
    host: Option<&str>,
    peer_ip: IpAddr,
    module: &str,
    error: &io::Error,
) {
    let display = format_host(host, peer_ip);
    let module_display = sanitize_module_identifier(module);
    let text = format!(
        "failed to update lock for module '{module_display}' requested from {display} ({peer_ip}): {error}"
    );
    let message = rsync_error!(FEATURE_UNAVAILABLE_EXIT_CODE, text).with_role(Role::Daemon);
    log_message(log, &message);
}

fn log_module_refused_option(
    log: &SharedLogSink,
    host: Option<&str>,
    peer_ip: IpAddr,
    module: &str,
    refused: &str,
) {
    let display = format_host(host, peer_ip);
    let module_display = sanitize_module_identifier(module);
    let text = format!(
        "refusing option '{refused}' for module '{module_display}' from {display} ({peer_ip})"
    );
    let message = rsync_info!(text).with_role(Role::Daemon);
    log_message(log, &message);
}

pub(crate) fn log_module_auth_failure(
    log: &SharedLogSink,
    host: Option<&str>,
    peer_ip: IpAddr,
    module: &str,
) {
    let display = format_host(host, peer_ip);
    let module_display = sanitize_module_identifier(module);
    // upstream: authenticate.c:249 / :335 - `auth failed on module %s from %s (%s)`.
    // Upstream appends the specific reason (`: invalid challenge response`, or
    // ` for %s: %s`); the failure reason is not propagated to this emission point,
    // so only the invariant prefix is reproduced here.
    let text = format!("auth failed on module {module_display} from {display} ({peer_ip})");
    let message = rsync_info!(text).with_role(Role::Daemon);
    log_message(log, &message);
}

pub(crate) fn log_module_denied(
    log: &SharedLogSink,
    host: Option<&str>,
    peer_ip: IpAddr,
    module: &str,
) {
    let display = format_host(host, peer_ip);
    let module_display = sanitize_module_identifier(module);
    // upstream: clientserver.c:729 - `rsync denied on module %s from %s (%s)`
    let text = format!("rsync denied on module {module_display} from {display} ({peer_ip})");
    let message = rsync_info!(text).with_role(Role::Daemon);
    log_message(log, &message);
}

pub(crate) fn log_unknown_module(
    log: &SharedLogSink,
    host: Option<&str>,
    peer_ip: IpAddr,
    module: &str,
) {
    let display = format_host(host, peer_ip);
    let module_display = sanitize_module_identifier(module);
    // upstream: clientserver.c:1434 - `unknown module '%s' tried from %s (%s)`
    let text = format!("unknown module '{module_display}' tried from {display} ({peer_ip})");
    let message = rsync_info!(text).with_role(Role::Daemon);
    log_message(log, &message);
}


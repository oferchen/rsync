/// Logs a bandwidth limiter change for a module connection.
///
/// No-op when `change` is [`LimiterChange::Unchanged`].
pub(crate) fn log_module_bandwidth_change(
    log: &SharedLogSink,
    host: Option<&str>,
    peer_ip: IpAddr,
    module: &str,
    limiter: Option<&BandwidthLimiter>,
    change: LimiterChange,
) {
    if change == LimiterChange::Unchanged {
        return;
    }

    let display = format_host(host, peer_ip);
    let module_display = sanitize_module_identifier(module);

    let message = match change {
        LimiterChange::Unchanged => return,
        LimiterChange::Disabled => {
            let text = format!(
                "removed bandwidth limit for module '{module_display}' requested from {display} ({peer_ip})"
            );
            rsync_info!(text).with_role(Role::Daemon)
        }
        LimiterChange::Enabled | LimiterChange::Updated => {
            let Some(limiter) = limiter else {
                return;
            };
            let limit = format_bandwidth_rate(limiter.limit_bytes());
            let burst = limiter
                .burst_bytes()
                .map(|value| format!(" with burst {}", format_bandwidth_rate(value)))
                .unwrap_or_default();
            let action = match change {
                LimiterChange::Enabled => "enabled",
                LimiterChange::Updated => "updated",
                LimiterChange::Disabled | LimiterChange::Unchanged => unreachable!(),
            };
            let text = format!(
                "{action} bandwidth limit {limit}{burst} for module '{module_display}' requested from {display} ({peer_ip})"
            );
            rsync_info!(text).with_role(Role::Daemon)
        }
    };

    log_message(log, &message);
}

fn log_connection(log: &SharedLogSink, host: Option<&str>, peer_addr: SocketAddr) {
    let display = format_host(host, peer_addr.ip());
    let ip = peer_addr.ip();
    let text = format!("connect from {display} ({ip})");
    let message = rsync_info!(text).with_role(Role::Daemon);
    log_message(log, &message);
}

fn log_list_request(log: &SharedLogSink, host: Option<&str>, peer_addr: SocketAddr) {
    let display = format_host(host, peer_addr.ip());
    let ip = peer_addr.ip();
    let text = format!("list request from {display} ({ip})");
    let message = rsync_info!(text).with_role(Role::Daemon);
    log_message(log, &message);
}

fn log_module_request(log: &SharedLogSink, host: Option<&str>, peer_ip: IpAddr, module: &str) {
    let display = format_host(host, peer_ip);
    let module_display = sanitize_module_identifier(module);
    let text = format!(
        "module '{module_display}' requested from {display} ({peer_ip})"
    );
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
/// `peer` records the rejected client IP, `cap` is the limit that
/// triggered the refusal, and `current` is the active connection count
/// observed at the refusal moment.
pub(crate) fn log_module_limit(
    log: &SharedLogSink,
    host: Option<&str>,
    peer_ip: IpAddr,
    module: &str,
    limit: NonZeroU32,
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

fn log_module_auth_failure(log: &SharedLogSink, host: Option<&str>, peer_ip: IpAddr, module: &str) {
    let display = format_host(host, peer_ip);
    let module_display = sanitize_module_identifier(module);
    let text = format!(
        "authentication failed for module '{module_display}' from {display} ({peer_ip})"
    );
    let message = rsync_info!(text).with_role(Role::Daemon);
    log_message(log, &message);
}

fn log_module_auth_success(log: &SharedLogSink, host: Option<&str>, peer_ip: IpAddr, module: &str) {
    let display = format_host(host, peer_ip);
    let module_display = sanitize_module_identifier(module);
    let text = format!(
        "authentication succeeded for module '{module_display}' from {display} ({peer_ip})"
    );
    let message = rsync_info!(text).with_role(Role::Daemon);
    log_message(log, &message);
}

fn log_module_denied(log: &SharedLogSink, host: Option<&str>, peer_ip: IpAddr, module: &str) {
    let display = format_host(host, peer_ip);
    let module_display = sanitize_module_identifier(module);
    let text = format!(
        "access denied to module '{module_display}' from {display} ({peer_ip})"
    );
    let message = rsync_info!(text).with_role(Role::Daemon);
    log_message(log, &message);
}

fn log_unknown_module(log: &SharedLogSink, host: Option<&str>, peer_ip: IpAddr, module: &str) {
    let display = format_host(host, peer_ip);
    let module_display = sanitize_module_identifier(module);
    let text = format!(
        "unknown module '{module_display}' requested from {display} ({peer_ip})"
    );
    let message = rsync_info!(text).with_role(Role::Daemon);
    log_message(log, &message);
}


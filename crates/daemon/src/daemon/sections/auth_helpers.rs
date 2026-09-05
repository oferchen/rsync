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

pub(crate) fn log_module_request(log: &SharedLogSink, host: &str, peer_ip: IpAddr, module: &str) {
    // upstream: clientserver.c:787 - `rsync allowed access on module %s from %s (%s)`
    let text = format!("rsync allowed access on module {module} from {host} ({peer_ip})");
    let message = rsync_info!(text).with_role(Role::Daemon);
    log_message(log, &message);
}

/// Emits a structured warning when a module rejects a connection because
/// its per-module `max connections` cap has been reached.
///
/// Mirrors the global cap warning emitted from
/// [`log_max_connections_rejection`] so operators see one consistent
/// shape across both admission paths. Fields are stable and named:
/// `which` carries the module name verbatim - the log sink escapes it,
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
    let text = format!(
        "max-connections cap reached: which={module} peer={host} ({peer_ip}) cap={limit} current={current}"
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
    let text = format!(
        "failed to update lock for module '{module}' requested from {host} ({peer_ip}): {error}"
    );
    let message = rsync_error!(FEATURE_UNAVAILABLE_EXIT_CODE, text).with_role(Role::Daemon);
    log_message(log, &message);
}

/// The refusal text upstream builds once and delivers to BOTH the peer and the
/// daemon log.
///
/// upstream: `options.c:1409-1423` `create_refuse_error()` fills `err_buf` with
/// `"The server is configured to refuse --<longName>\n"`, and the post-`@RSYNCD:
/// OK` failure branch at `clientserver.c:1254` reaches `option_error()`
/// (`options.c:907-918`), which emits that same buffer via
/// `rprintf(FERROR, RSYNC_NAME ": %s", err_buf)`. A daemon's `FERROR` lands in
/// the log file, so upstream has ONE string serving as both the peer's `@ERROR`
/// payload and the logged line.
///
/// This is a single owner for exactly that reason: oc previously formatted the
/// peer's text and the log's text independently, and they drifted - the peer
/// saw upstream's wording while the log carried an oc-invented
/// `refusing option '...' for module '...'` line that no upstream site emits.
pub(crate) fn refused_option_message(refused: &str) -> String {
    format!("The server is configured to refuse {refused}")
}

/// Logs a refused option in upstream's words.
///
/// upstream: `options.c:915` - `rprintf(FERROR, RSYNC_NAME ": %s", err_buf)`.
/// The logged line therefore carries the `rsync: ` prefix `option_error()` adds
/// and nothing more: upstream names no module, host or peer here, because the
/// connection is already identified by the line's pid stamp and by the
/// preceding `rsync allowed access on module %s from %s (%s)`
/// (`clientserver.c:787`), which oc emits faithfully.
///
/// The log CODE is deliberately left as it was. Upstream routes this at
/// `FERROR` while oc records it at info level; both reach the daemon log file,
/// which is the observable this mirrors, and re-coding it would change oc's
/// message routing for a reason this change has not measured.
fn log_module_refused_option(log: &SharedLogSink, refused: &str) {
    let text = format!("rsync: {}", refused_option_message(refused));
    let message = rsync_info!(text).with_role(Role::Daemon);
    log_message(log, &message);
}

/// Logs a failure to read the client's argument vector.
///
/// upstream: `io.c:1477-1478` - `read_args()` reports the refusal where it
/// happens, e.g. `rprintf(FERROR, "too many daemon arguments\n")`, and a
/// daemon's `FERROR` reaches the log file. oc sent that refusal to the peer and
/// logged nothing at all, so an operator watching the log could not see why a
/// connection was cut - the guard fired invisibly.
///
/// The text is the error's own and is emitted bare: unlike `option_error()`
/// (`options.c:915`) upstream adds no `rsync: ` prefix at this site.
fn log_client_args_failure(log: &SharedLogSink, error: &io::Error) {
    let message = rsync_info!(error.to_string()).with_role(Role::Daemon);
    log_message(log, &message);
}

pub(crate) fn log_module_auth_failure(
    log: &SharedLogSink,
    host: &str,
    peer_ip: IpAddr,
    module: &str,
    suffix: Option<&str>,
) {
    // upstream: `auth failed on module %s from %s (%s)` is the common prefix;
    // what follows differs by refusal. authenticate.c:318 / :325 append `: %s`
    // for the digest-floor arms, and :433 appends ` for %s: %s` once a username
    // is known. `AuthDenial::log_suffix` picks between those shapes, so this
    // layer just concatenates - one owner for the prefix, one for the tail.
    let text = match suffix {
        Some(suffix) => {
            format!("auth failed on module {module} from {host} ({peer_ip}){suffix}")
        }
        None => format!("auth failed on module {module} from {host} ({peer_ip})"),
    };
    let message = rsync_info!(text).with_role(Role::Daemon);
    log_message(log, &message);
}

pub(crate) fn log_module_denied(log: &SharedLogSink, host: &str, peer_ip: IpAddr, module: &str) {
    // upstream: clientserver.c:774 - `rsync denied on module %s from %s (%s)`
    let text = format!("rsync denied on module {module} from {host} ({peer_ip})");
    let message = rsync_info!(text).with_role(Role::Daemon);
    log_message(log, &message);
}

pub(crate) fn log_unknown_module(log: &SharedLogSink, host: &str, peer_ip: IpAddr, module: &str) {
    // upstream: clientserver.c:1568 - `unknown module '%s' tried from %s (%s)`
    let text = format!("unknown module '{module}' tried from {host} ({peer_ip})");
    let message = rsync_info!(text).with_role(Role::Daemon);
    log_message(log, &message);
}

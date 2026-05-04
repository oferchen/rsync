/// Re-reads and re-parses the daemon configuration file on SIGHUP.
///
/// On success, replaces `modules` and `motd_lines` with freshly parsed values
/// so that subsequent connections use the new configuration. Existing
/// connections retain the old config via their `Arc` clones.
///
/// On failure (missing file, parse error), the error is logged and the daemon
/// continues with the previous configuration - matching upstream rsync
/// behaviour where a bad config reload is non-fatal.
///
/// upstream: clientserver.c - `re_read_config()` called from SIGHUP handler.
fn reload_daemon_config(
    config_path: Option<&Path>,
    connection_limiter: &Option<Arc<ConnectionLimiter>>,
    modules: &mut Arc<Vec<ModuleRuntime>>,
    motd_lines: &mut Arc<Vec<String>>,
    log_sink: Option<&SharedLogSink>,
    notifier: &systemd::ServiceNotifier,
) {
    if let Some(log) = log_sink {
        let message = rsync_info!("received SIGHUP, reloading configuration")
            .with_role(Role::Daemon);
        log_message(log, &message);
    }
    if let Err(error) = notifier.status("Reloading configuration") {
        log_sd_notify_failure(log_sink, "config reload status", &error);
    }

    let path = match config_path {
        Some(path) => path,
        None => {
            if let Some(log) = log_sink {
                let message = rsync_info!(
                    "SIGHUP ignored: no config file was loaded at startup"
                )
                .with_role(Role::Daemon);
                log_message(log, &message);
            }
            return;
        }
    };

    let parsed = match parse_config_modules(path) {
        Ok(parsed) => parsed,
        Err(error) => {
            if let Some(log) = log_sink {
                let text = format!(
                    "config reload failed, keeping old configuration: {error}"
                );
                let message = rsync_warning!(text).with_role(Role::Daemon);
                log_message(log, &message);
            }
            return;
        }
    };

    let new_modules: Vec<ModuleRuntime> = parsed
        .modules
        .into_iter()
        .map(|definition| ModuleRuntime::new(definition, connection_limiter.clone()))
        .collect();
    let module_count = new_modules.len();
    *modules = Arc::new(new_modules);
    *motd_lines = Arc::new(parsed.motd_lines);

    if let Some(log) = log_sink {
        let text = format!(
            "configuration reloaded successfully ({module_count} module{})",
            if module_count == 1 { "" } else { "s" }
        );
        let message = rsync_info!(text).with_role(Role::Daemon);
        log_message(log, &message);
    }

    let status = format!("Configuration reloaded ({module_count} modules)");
    if let Err(error) = notifier.status(&status) {
        log_sd_notify_failure(log_sink, "config reload status", &error);
    }
}

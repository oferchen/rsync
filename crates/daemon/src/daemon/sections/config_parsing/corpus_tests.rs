// Corpus gate: oc must read every rsyncd.conf in the corpus the way upstream
// rsync 3.5.1's loadparm.c does.

#[cfg(all(test, unix))]
mod rsyncd_conf_corpus_tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::fmt::Write as _;

    /// upstream: loadparm.c parameter values of one section, as the corpus
    /// dumper printed them through the lp_*() accessors.
    type Section = BTreeMap<String, String>;

    struct UpstreamDump {
        globals: Section,
        modules: Vec<Section>,
    }

    fn corpus_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/rsyncd_conf_corpus")
    }

    fn read_dump(path: &Path) -> UpstreamDump {
        let text = fs::read_to_string(path).expect("read upstream.dump");
        let mut lines = text.lines();
        assert_eq!(
            lines.next(),
            Some("load=OK"),
            "{}: upstream failed to load",
            path.display()
        );
        let mut globals = Section::new();
        let mut modules = Vec::new();
        let mut current = &mut globals;
        for line in lines {
            if line == "[global]" {
                continue;
            }
            if line.starts_with("[module ") {
                modules.push(Section::new());
                current = modules.last_mut().expect("just pushed");
                continue;
            }
            let (key, value) = line.split_once('=').expect("key=value line");
            current.insert(key.to_owned(), value.to_owned());
        }
        UpstreamDump { globals, modules }
    }

    fn opt_str(value: Option<&str>) -> String {
        value.unwrap_or_default().to_owned()
    }

    fn opt_path(value: Option<&Path>) -> String {
        value
            .map(|path| path.display().to_string())
            .unwrap_or_default()
    }

    fn boolean(value: bool) -> String {
        u8::from(value).to_string()
    }

    /// upstream: BOOL3 prints -1 for Unset.
    fn tri_state(value: Option<bool>) -> String {
        value.map_or_else(|| "-1".to_owned(), boolean)
    }

    /// The `LOG_*` value of a facility name as stored by the daemon.
    fn syslog_facility_code(value: Option<&str>) -> String {
        const NAMES: &[(&str, i32)] = &[
            ("kern", 0),
            ("user", 1),
            ("mail", 2),
            ("daemon", 3),
            ("auth", 4),
            ("security", 4),
            ("syslog", 5),
            ("lpr", 6),
            ("news", 7),
            ("uucp", 8),
            ("cron", 9),
            ("authpriv", 10),
            ("ftp", 11),
            ("local0", 16),
            ("local1", 17),
            ("local2", 18),
            ("local3", 19),
            ("local4", 20),
            ("local5", 21),
            ("local6", 22),
            ("local7", 23),
        ];
        let value = value.unwrap_or("daemon");
        NAMES
            .iter()
            .find(|(name, _)| *name == value)
            .map_or_else(|| value.to_owned(), |(_, code)| (code << 3).to_string())
    }

    /// Renders what oc resolved for one module in the dump's vocabulary, with
    /// each upstream value run through the oc parser for that parameter so a
    /// difference means a different parse, not a different spelling.
    fn compare_module(
        oc: &ModuleDefinition,
        global_lock_file: Option<&Path>,
        upstream: &Section,
        diffs: &mut String,
    ) {
        let get = |key: &str| upstream.get(key).map(String::as_str).unwrap_or_default();
        let mut check = |key: &str, ours: String, theirs: String| {
            if ours != theirs {
                let _ = writeln!(
                    diffs,
                    "  [{}] {key}: oc={ours:?} upstream={theirs:?}",
                    oc.name
                );
            }
        };

        check("name", oc.name.clone(), get("name").to_owned());
        check(
            "path",
            oc.path.display().to_string(),
            get("path").to_owned(),
        );
        check(
            "comment",
            opt_str(oc.comment.as_deref()),
            get("comment").to_owned(),
        );
        check(
            "read_only",
            boolean(oc.read_only),
            get("read_only").to_owned(),
        );
        check(
            "write_only",
            boolean(oc.write_only),
            get("write_only").to_owned(),
        );
        check("list", boolean(oc.listable), get("list").to_owned());
        check(
            "fake_super",
            boolean(oc.fake_super),
            get("fake_super").to_owned(),
        );
        check(
            "forward_lookup",
            boolean(oc.forward_lookup),
            get("forward_lookup").to_owned(),
        );
        check(
            "ignore_errors",
            boolean(oc.ignore_errors),
            get("ignore_errors").to_owned(),
        );
        check(
            "ignore_nonreadable",
            boolean(oc.ignore_nonreadable),
            get("ignore_nonreadable").to_owned(),
        );
        check(
            "insecure_links",
            boolean(oc.insecure_links),
            get("insecure_links").to_owned(),
        );
        check(
            "reverse_lookup",
            boolean(oc.reverse_lookup),
            get("reverse_lookup").to_owned(),
        );
        check(
            "strict_modes",
            boolean(oc.strict_modes),
            get("strict_modes").to_owned(),
        );
        check(
            "transfer_logging",
            boolean(oc.transfer_logging),
            get("transfer_logging").to_owned(),
        );
        check(
            "open_noatime",
            boolean(oc.open_noatime),
            boolean(get("open_noatime") == "1"),
        );
        check(
            "use_chroot",
            if oc.use_chroot_explicit {
                boolean(oc.use_chroot)
            } else {
                "-1".to_owned()
            },
            get("use_chroot").to_owned(),
        );
        check(
            "munge_symlinks",
            tri_state(oc.munge_symlinks),
            get("munge_symlinks").to_owned(),
        );
        check(
            "numeric_ids",
            tri_state(oc.numeric_ids),
            get("numeric_ids").to_owned(),
        );
        check(
            "max_connections",
            format!("{:?}", oc.max_connections),
            format!(
                "{:?}",
                MaxConnections::from_configured(parse_atoi(get("max_connections")))
            ),
        );
        check(
            "max_verbosity",
            oc.max_verbosity.to_string(),
            get("max_verbosity").to_owned(),
        );
        check(
            "timeout",
            oc.timeout.map_or(0, NonZeroU64::get).to_string(),
            parse_atoi(get("timeout")).max(0).to_string(),
        );
        check(
            "hosts_allow",
            format!("{:?}", oc.hosts_allow),
            format!("{:?}", parse_host_list(get("hosts_allow"))),
        );
        check(
            "hosts_deny",
            format!("{:?}", oc.hosts_deny),
            format!("{:?}", parse_host_list(get("hosts_deny"))),
        );
        check(
            "auth_users",
            format!("{:?}", oc.auth_users),
            format!(
                "{:?}",
                parse_auth_user_list(get("auth_users")).unwrap_or_default()
            ),
        );
        check(
            "auth_digest",
            format!("{:?}", oc.auth_digest),
            format!("{:?}", normalize_auth_digest(get("auth_digest"))),
        );
        check(
            "refuse_options",
            format!("{:?}", oc.refuse_options),
            format!(
                "{:?}",
                parse_refuse_option_list(get("refuse_options")).unwrap_or_default()
            ),
        );
        check(
            "secrets_file",
            opt_path(oc.secrets_file.as_deref()),
            get("secrets_file").to_owned(),
        );
        check(
            "dont_compress",
            opt_str(oc.dont_compress.as_deref()),
            get("dont_compress").to_owned(),
        );
        check(
            "log_format",
            opt_str(oc.log_format.as_deref()),
            get("log_format").to_owned(),
        );
        check(
            "log_file",
            opt_path(oc.log_file.as_deref()),
            get("log_file").to_owned(),
        );
        check("exclude", oc.exclude.join(" "), get("exclude").to_owned());
        check("include", oc.include.join(" "), get("include").to_owned());
        check("filter", oc.filter.join(" "), get("filter").to_owned());
        check(
            "exclude_from",
            opt_path(oc.exclude_from.as_deref()),
            get("exclude_from").to_owned(),
        );
        check(
            "include_from",
            opt_path(oc.include_from.as_deref()),
            get("include_from").to_owned(),
        );
        check(
            "temp_dir",
            opt_str(oc.temp_dir.as_deref()),
            get("temp_dir").to_owned(),
        );
        check(
            "charset",
            opt_str(oc.charset.as_deref()),
            get("charset").to_owned(),
        );
        check(
            "early_exec",
            opt_str(oc.early_exec.as_deref()),
            get("early_exec").to_owned(),
        );
        check(
            "prexfer_exec",
            opt_str(oc.pre_xfer_exec.as_deref()),
            get("prexfer_exec").to_owned(),
        );
        check(
            "postxfer_exec",
            opt_str(oc.post_xfer_exec.as_deref()),
            get("postxfer_exec").to_owned(),
        );
        check(
            "name_converter",
            opt_str(oc.name_converter.as_deref()),
            get("name_converter").to_owned(),
        );
        check(
            "incoming_chmod",
            opt_str(oc.incoming_chmod.as_deref()),
            get("incoming_chmod").to_owned(),
        );
        check(
            "outgoing_chmod",
            opt_str(oc.outgoing_chmod.as_deref()),
            get("outgoing_chmod").to_owned(),
        );
        check(
            "syslog_tag",
            oc.syslog_tag.clone().unwrap_or_else(|| "rsyncd".to_owned()),
            get("syslog_tag").to_owned(),
        );
        check(
            "syslog_facility",
            syslog_facility_code(oc.syslog_facility.as_deref()),
            get("syslog_facility").to_owned(),
        );
        check(
            "lock_file",
            oc.lock_file
                .as_deref()
                .or(global_lock_file)
                .unwrap_or(Path::new(DEFAULT_LOCK_FILE))
                .display()
                .to_string(),
            get("lock_file").to_owned(),
        );

        let uid = get("uid");
        let (ours, theirs) = if uid.is_empty() {
            (
                format!("{:?}/{:?}", oc.uid, oc.unresolved_id),
                "None/None".to_owned(),
            )
        } else {
            let expected = parse_uid_setting(uid).map_or_else(
                || format!("None/{:?}", Some(UnresolvedId::Uid(uid.to_owned()))),
                |n| format!("{:?}/None", Some(n)),
            );
            let resolved = match &oc.unresolved_id {
                Some(UnresolvedId::Uid(_)) => format!("None/{:?}", oc.unresolved_id),
                _ => format!("{:?}/None", oc.uid),
            };
            (resolved, expected)
        };
        check("uid", ours, theirs);

        let gid = get("gid");
        let expected = if gid.is_empty() {
            "None".to_owned()
        } else {
            match parse_gid_setting(gid) {
                Ok(setting) => format!("{:?}", Some(setting)),
                Err(_) => format!("{:?}", UnresolvedId::Gid(rejected_gid_token(gid))),
            }
        };
        let ours = match &oc.unresolved_id {
            Some(gid @ UnresolvedId::Gid(_)) => format!("{gid:?}"),
            _ => format!("{:?}", oc.gid),
        };
        if !matches!(oc.unresolved_id, Some(UnresolvedId::Uid(_))) {
            check("gid", ours, expected);
        }
    }

    fn compare_globals(oc: &ParsedConfigModules, upstream: &Section, diffs: &mut String) {
        let get = |key: &str| upstream.get(key).map(String::as_str).unwrap_or_default();
        let mut check = |key: &str, ours: String, theirs: String| {
            if ours != theirs {
                let _ = writeln!(diffs, "  [global] {key}: oc={ours:?} upstream={theirs:?}");
            }
        };
        let path = |slot: &Option<(PathBuf, ConfigDirectiveOrigin)>| {
            slot.as_ref()
                .map(|(p, _)| p.display().to_string())
                .unwrap_or_default()
        };
        let text = |slot: &Option<(String, ConfigDirectiveOrigin)>| {
            slot.as_ref().map(|(s, _)| s.clone()).unwrap_or_default()
        };
        check("pid_file", path(&oc.pid_file), get("pid_file").to_owned());
        check(
            "daemon_chroot",
            path(&oc.daemon_chroot),
            get("daemon_chroot").to_owned(),
        );
        check(
            "daemon_uid",
            text(&oc.daemon_uid),
            get("daemon_uid").to_owned(),
        );
        check(
            "daemon_gid",
            text(&oc.daemon_gid),
            get("daemon_gid").to_owned(),
        );
        check(
            "socket_options",
            text(&oc.socket_options),
            get("socket_options").to_owned(),
        );
        check(
            "bind_address",
            oc.bind_address
                .as_ref()
                .map(|(a, _)| a.to_string())
                .unwrap_or_default(),
            get("bind_address").to_owned(),
        );
        check(
            "listen_backlog",
            oc.listen_backlog
                .as_ref()
                .map_or(5, |(n, _)| *n)
                .to_string(),
            get("listen_backlog").to_owned(),
        );
        check(
            "rsync_port",
            oc.rsync_port.as_ref().map_or(0, |(n, _)| *n).to_string(),
            get("rsync_port").to_owned(),
        );
        check(
            "proxy_protocol",
            boolean(oc.proxy_protocol.as_ref().is_some_and(|(b, _)| *b)),
            get("proxy_protocol").to_owned(),
        );
        check(
            "proxy_protocol_hosts",
            format!(
                "{:?}",
                oc.proxy_protocol_hosts
                    .as_ref()
                    .map(|(h, _)| h.clone())
                    .unwrap_or_default()
            ),
            format!("{:?}", parse_host_list(get("proxy_protocol_hosts"))),
        );
        check("log_file", path(&oc.log_file), get("log_file").to_owned());
        check(
            "timeout",
            oc.daemon_timeout
                .as_ref()
                .and_then(|(t, _)| *t)
                .map_or(0, NonZeroU64::get)
                .to_string(),
            parse_atoi(get("timeout")).max(0).to_string(),
        );
        check(
            "reverse_lookup",
            boolean(oc.reverse_lookup.as_ref().is_none_or(|(b, _)| *b)),
            get("reverse_lookup").to_owned(),
        );
    }

    #[test]
    fn every_corpus_config_loads_and_parses_as_upstream_does() {
        let mut entries: Vec<PathBuf> = fs::read_dir(corpus_dir())
            .expect("read corpus dir")
            .map(|entry| entry.expect("corpus entry").path())
            .filter(|path| path.join("rsyncd.conf").is_file())
            .collect();
        entries.sort();
        assert!(
            entries.len() >= 100,
            "corpus shrank to {} entries",
            entries.len()
        );

        let mut report = String::new();
        for entry in &entries {
            let upstream = read_dump(&entry.join("upstream.dump"));
            let parsed = match parse_config_modules(&entry.join("rsyncd.conf")) {
                Ok(parsed) => parsed,
                Err(error) => {
                    let _ = writeln!(
                        report,
                        "{}: oc refused the config: {error}",
                        entry.display()
                    );
                    continue;
                }
            };
            let mut diffs = String::new();
            if parsed.modules.len() != upstream.modules.len() {
                let _ = writeln!(
                    diffs,
                    "  module count: oc={} upstream={}",
                    parsed.modules.len(),
                    upstream.modules.len()
                );
            }
            let global_lock = parsed.lock_file.as_ref().map(|(p, _)| p.as_path());
            for (oc, theirs) in parsed.modules.iter().zip(&upstream.modules) {
                compare_module(oc, global_lock, theirs, &mut diffs);
            }
            compare_globals(&parsed, &upstream.globals, &mut diffs);
            if !diffs.is_empty() {
                let _ = writeln!(report, "{}:\n{diffs}", entry.display());
            }
        }
        assert!(
            report.is_empty(),
            "configs oc reads differently from upstream:\n{report}"
        );
    }
}

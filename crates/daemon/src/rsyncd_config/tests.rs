use super::*;
use std::io::Write;
use std::path::Path;
use tempfile::NamedTempFile;

fn write_config(content: &str) -> NamedTempFile {
    let mut file = NamedTempFile::new().expect("create temp file");
    file.write_all(content.as_bytes()).expect("write config");
    file.flush().expect("flush");
    file
}

#[test]
fn parse_empty_config() {
    let file = write_config("");
    let config = RsyncdConfig::from_file(file.path()).expect("parse succeeds");
    assert_eq!(config.global().port(), 873);
    assert!(config.modules().is_empty());
}

#[test]
fn parse_global_parameters() {
    let file = write_config(
        "port = 8873\n\
         address = 127.0.0.1\n\
         motd file = /etc/motd\n\
         log file = /var/log/rsyncd.log\n\
         pid file = /var/run/rsyncd.pid\n",
    );
    let config = RsyncdConfig::from_file(file.path()).expect("parse succeeds");
    assert_eq!(config.global().port(), 8873);
    assert_eq!(config.global().address(), Some("127.0.0.1"));
    assert_eq!(config.global().motd_file(), Some(Path::new("/etc/motd")));
    assert_eq!(
        config.global().log_file(),
        Some(Path::new("/var/log/rsyncd.log"))
    );
    assert_eq!(
        config.global().pid_file(),
        Some(Path::new("/var/run/rsyncd.pid"))
    );
}

#[test]
fn parse_minimal_module() {
    let file = write_config("[mymodule]\npath = /data/mymodule\n");
    let config = RsyncdConfig::from_file(file.path()).expect("parse succeeds");
    assert_eq!(config.modules().len(), 1);

    let module = &config.modules()[0];
    assert_eq!(module.name(), "mymodule");
    assert_eq!(module.path(), Path::new("/data/mymodule"));
    assert!(module.read_only());
    assert!(!module.write_only());
    assert!(module.list());
    assert!(module.use_chroot());
    assert!(!module.numeric_ids());
}

#[test]
fn parse_full_module() {
    let file = write_config(
        "[test]\n\
         path = /srv/test\n\
         comment = Test module\n\
         read only = no\n\
         write only = yes\n\
         list = no\n\
         uid = nobody\n\
         gid = nogroup\n\
         max connections = 10\n\
         lock file = /var/lock/rsync\n\
         auth users = user1, user2\n\
         secrets file = /etc/rsyncd.secrets\n\
         hosts allow = 192.168.1.0/24\n\
         hosts deny = *\n\
         exclude = .git/\n\
         include = *.txt\n\
         filter = - *.tmp\n\
         timeout = 300\n\
         use chroot = no\n\
         numeric ids = yes\n\
         fake super = yes\n\
         transfer logging = yes\n\
         refuse options = delete, hardlinks\n\
         dont compress = *.zip *.gz\n\
         early exec = /usr/local/bin/early-check\n\
         pre-xfer exec = /usr/local/bin/pre-xfer\n\
         post-xfer exec = /usr/local/bin/post-xfer\n\
         name converter = /usr/local/bin/nameconv\n",
    );
    let config = RsyncdConfig::from_file(file.path()).expect("parse succeeds");
    let module = &config.modules()[0];

    assert_eq!(module.name(), "test");
    assert_eq!(module.path(), Path::new("/srv/test"));
    assert_eq!(module.comment(), Some("Test module"));
    assert!(!module.read_only());
    assert!(module.write_only());
    assert!(!module.list());
    assert_eq!(module.uid(), Some("nobody"));
    assert_eq!(module.gid(), Some("nogroup"));
    assert_eq!(module.max_connections(), 10);
    assert_eq!(module.lock_file(), Some(Path::new("/var/lock/rsync")));
    assert_eq!(module.auth_users(), &["user1", "user2"]);
    assert_eq!(
        module.secrets_file(),
        Some(Path::new("/etc/rsyncd.secrets"))
    );
    assert_eq!(module.hosts_allow(), &["192.168.1.0/24"]);
    assert_eq!(module.hosts_deny(), &["*"]);
    assert_eq!(module.exclude(), &[".git/"]);
    assert_eq!(module.include(), &["*.txt"]);
    assert_eq!(module.filter(), &["- *.tmp"]);
    assert_eq!(module.timeout(), Some(300));
    assert!(!module.use_chroot());
    assert!(module.numeric_ids());
    assert!(module.fake_super());
    assert!(module.transfer_logging());
    assert_eq!(module.refuse_options(), &["delete", "hardlinks"]);
    assert_eq!(module.dont_compress(), &["*.zip", "*.gz"]);
    assert_eq!(module.early_exec(), Some("/usr/local/bin/early-check"));
    assert_eq!(module.pre_xfer_exec(), Some("/usr/local/bin/pre-xfer"));
    assert_eq!(module.post_xfer_exec(), Some("/usr/local/bin/post-xfer"));
    assert_eq!(module.name_converter(), Some("/usr/local/bin/nameconv"));
}

#[test]
fn parse_multiple_modules() {
    let file = write_config(
        "[mod1]\npath = /data/mod1\n\n\
         [mod2]\npath = /data/mod2\ncomment = Second module\n",
    );
    let config = RsyncdConfig::from_file(file.path()).expect("parse succeeds");
    assert_eq!(config.modules().len(), 2);
    assert_eq!(config.modules()[0].name(), "mod1");
    assert_eq!(config.modules()[1].name(), "mod2");
    assert_eq!(config.modules()[1].comment(), Some("Second module"));
}

#[test]
fn parse_comments_and_blank_lines() {
    let file = write_config(
        "# This is a comment\n\
         ; This is also a comment\n\
         \n\
         port = 873\n\
         \n\
         [module]\n\
         # Module comment\n\
         path = /data\n",
    );
    let config = RsyncdConfig::from_file(file.path()).expect("parse succeeds");
    assert_eq!(config.global().port(), 873);
    assert_eq!(config.modules().len(), 1);
}

#[test]
fn parse_boolean_values() {
    for value in ["yes", "true", "1", "YES", "True"] {
        let file = write_config(&format!("[mod]\npath = /data\nread only = {value}\n"));
        let config = RsyncdConfig::from_file(file.path()).expect("parse succeeds");
        assert!(config.modules()[0].read_only());
    }

    for value in ["no", "false", "0", "NO", "False"] {
        let file = write_config(&format!("[mod]\npath = /data\nread only = {value}\n"));
        let config = RsyncdConfig::from_file(file.path()).expect("parse succeeds");
        assert!(!config.modules()[0].read_only());
    }
}

#[test]
fn parse_list_values() {
    let file = write_config("[mod]\npath = /data\nauth users = alice, bob, charlie\n");
    let config = RsyncdConfig::from_file(file.path()).expect("parse succeeds");
    assert_eq!(
        config.modules()[0].auth_users(),
        &["alice", "bob", "charlie"]
    );
}

#[test]
fn get_module_by_name() {
    let file = write_config(
        "[first]\npath = /data/first\n\
         [second]\npath = /data/second\n",
    );
    let config = RsyncdConfig::from_file(file.path()).expect("parse succeeds");

    assert!(config.get_module("first").is_some());
    assert!(config.get_module("second").is_some());
    assert!(config.get_module("nonexistent").is_none());
}

#[test]
fn error_missing_path() {
    let file = write_config("[module]\ncomment = Missing path\n");
    let err = RsyncdConfig::from_file(file.path()).expect_err("should fail");
    assert!(err.to_string().contains("missing required 'path'"));
    assert_eq!(err.line(), Some(1));
}

#[test]
fn error_unterminated_header() {
    let file = write_config("[module\npath = /data\n");
    let err = RsyncdConfig::from_file(file.path()).expect_err("should fail");
    assert!(err.to_string().contains("unterminated module header"));
    assert_eq!(err.line(), Some(1));
}

#[test]
fn error_empty_module_name() {
    let file = write_config("[]\npath = /data\n");
    let err = RsyncdConfig::from_file(file.path()).expect_err("should fail");
    assert!(err.to_string().contains("module name must be non-empty"));
}

#[test]
fn error_invalid_boolean() {
    let file = write_config("[mod]\npath = /data\nread only = maybe\n");
    let err = RsyncdConfig::from_file(file.path()).expect_err("should fail");
    assert!(err.to_string().contains("invalid boolean"));
}

#[test]
fn error_invalid_port() {
    let file = write_config("port = not_a_number\n");
    let err = RsyncdConfig::from_file(file.path()).expect_err("should fail");
    assert!(err.to_string().contains("invalid port"));
}

#[test]
fn error_duplicate_module() {
    let file = write_config(
        "[module]\npath = /data/one\n\
         [module]\npath = /data/two\n",
    );
    let err = RsyncdConfig::from_file(file.path()).expect_err("should fail");
    assert!(err.to_string().contains("duplicate module"));
}

#[test]
fn error_missing_equals() {
    let file = write_config("[mod]\npath /data\n");
    let err = RsyncdConfig::from_file(file.path()).expect_err("should fail");
    assert!(err.to_string().contains("key = value"));
}

#[test]
fn error_empty_path() {
    let file = write_config("[mod]\npath = \n");
    let err = RsyncdConfig::from_file(file.path()).expect_err("should fail");
    assert!(err.to_string().contains("path must not be empty"));
}

#[test]
fn error_empty_secrets_file() {
    let file = write_config("[mod]\npath = /data\nsecrets file = \n");
    let err = RsyncdConfig::from_file(file.path()).expect_err("should fail");
    assert!(err.to_string().contains("secrets file must not be empty"));
}

#[test]
fn trailing_comment_after_header() {
    let file = write_config("[module] # This is a comment\npath = /data\n");
    let config = RsyncdConfig::from_file(file.path()).expect("parse succeeds");
    assert_eq!(config.modules().len(), 1);
}

#[test]
fn trailing_text_after_header_errors() {
    let file = write_config("[module] extra text\npath = /data\n");
    let err = RsyncdConfig::from_file(file.path()).expect_err("should fail");
    assert!(err.to_string().contains("unexpected content"));
}

#[test]
fn case_insensitive_keys() {
    let file = write_config("[MOD]\nPATH = /data\nREAD ONLY = NO\n");
    let config = RsyncdConfig::from_file(file.path()).expect("parse succeeds");
    assert_eq!(config.modules()[0].path(), Path::new("/data"));
    assert!(!config.modules()[0].read_only());
}

#[test]
fn default_values() {
    let file = write_config("[mod]\npath = /data\n");
    let config = RsyncdConfig::from_file(file.path()).expect("parse succeeds");
    let module = &config.modules()[0];

    assert!(module.read_only());
    assert!(!module.write_only());
    assert!(module.list());
    assert_eq!(module.max_connections(), 0);
    assert!(module.use_chroot());
    assert!(!module.numeric_ids());
    assert!(!module.fake_super());
    assert!(!module.transfer_logging());
    assert!(module.auth_users().is_empty());
    assert!(module.refuse_options().is_empty());
    assert!(module.dont_compress().is_empty());
    assert!(module.strict_modes());
    assert!(!module.open_noatime());
}

#[test]
fn config_error_display() {
    let file = write_config("[mod]\npath =\n");
    let err = RsyncdConfig::from_file(file.path()).expect_err("should fail");
    let display = err.to_string();
    assert!(display.contains("line 2"));
    assert!(display.contains("path must not be empty"));
}

#[test]
fn parse_with_global_and_modules() {
    let file = write_config(
        "port = 8873\n\
         log file = /var/log/rsync.log\n\
         \n\
         [public]\n\
         path = /srv/public\n\
         comment = Public files\n\
         read only = yes\n\
         \n\
         [upload]\n\
         path = /srv/upload\n\
         comment = Upload area\n\
         read only = no\n",
    );
    let config = RsyncdConfig::from_file(file.path()).expect("parse succeeds");

    assert_eq!(config.global().port(), 8873);
    assert_eq!(
        config.global().log_file(),
        Some(Path::new("/var/log/rsync.log"))
    );
    assert_eq!(config.modules().len(), 2);

    let public = config.get_module("public").unwrap();
    assert_eq!(public.comment(), Some("Public files"));
    assert!(public.read_only());

    let upload = config.get_module("upload").unwrap();
    assert_eq!(upload.comment(), Some("Upload area"));
    assert!(!upload.read_only());
}

#[test]
fn parse_strict_modes_yes() {
    let file = write_config("[mod]\npath = /data\nstrict modes = yes\n");
    let config = RsyncdConfig::from_file(file.path()).expect("parse succeeds");
    assert!(config.modules()[0].strict_modes());
}

#[test]
fn parse_strict_modes_no() {
    let file = write_config("[mod]\npath = /data\nstrict modes = no\n");
    let config = RsyncdConfig::from_file(file.path()).expect("parse succeeds");
    assert!(!config.modules()[0].strict_modes());
}

#[test]
fn parse_strict_modes_true() {
    let file = write_config("[mod]\npath = /data\nstrict modes = true\n");
    let config = RsyncdConfig::from_file(file.path()).expect("parse succeeds");
    assert!(config.modules()[0].strict_modes());
}

#[test]
fn parse_strict_modes_false() {
    let file = write_config("[mod]\npath = /data\nstrict modes = false\n");
    let config = RsyncdConfig::from_file(file.path()).expect("parse succeeds");
    assert!(!config.modules()[0].strict_modes());
}

#[test]
fn parse_strict_modes_default_true() {
    let file = write_config("[mod]\npath = /data\n");
    let config = RsyncdConfig::from_file(file.path()).expect("parse succeeds");
    assert!(config.modules()[0].strict_modes());
}

#[test]
fn parse_strict_modes_invalid_boolean() {
    let file = write_config("[mod]\npath = /data\nstrict modes = maybe\n");
    let err = RsyncdConfig::from_file(file.path()).expect_err("should fail");
    assert!(err.to_string().contains("invalid boolean"));
}

#[test]
fn parse_open_noatime_yes() {
    let file = write_config("[mod]\npath = /data\nopen noatime = yes\n");
    let config = RsyncdConfig::from_file(file.path()).expect("parse succeeds");
    assert!(config.modules()[0].open_noatime());
}

#[test]
fn parse_open_noatime_no() {
    let file = write_config("[mod]\npath = /data\nopen noatime = no\n");
    let config = RsyncdConfig::from_file(file.path()).expect("parse succeeds");
    assert!(!config.modules()[0].open_noatime());
}

#[test]
fn parse_open_noatime_true() {
    let file = write_config("[mod]\npath = /data\nopen noatime = true\n");
    let config = RsyncdConfig::from_file(file.path()).expect("parse succeeds");
    assert!(config.modules()[0].open_noatime());
}

#[test]
fn parse_open_noatime_false() {
    let file = write_config("[mod]\npath = /data\nopen noatime = false\n");
    let config = RsyncdConfig::from_file(file.path()).expect("parse succeeds");
    assert!(!config.modules()[0].open_noatime());
}

#[test]
fn parse_open_noatime_default_false() {
    let file = write_config("[mod]\npath = /data\n");
    let config = RsyncdConfig::from_file(file.path()).expect("parse succeeds");
    assert!(!config.modules()[0].open_noatime());
}

#[test]
fn parse_open_noatime_invalid_boolean() {
    let file = write_config("[mod]\npath = /data\nopen noatime = maybe\n");
    let err = RsyncdConfig::from_file(file.path()).expect_err("should fail");
    assert!(err.to_string().contains("invalid boolean"));
}

#[test]
fn global_directives_before_modules_are_parsed() {
    let file = write_config(
        "port = 9999\n\
         address = 10.0.0.1\n\
         motd file = /etc/motd.txt\n\
         log file = /var/log/daemon.log\n\
         pid file = /var/run/daemon.pid\n\
         socket options = SO_KEEPALIVE\n\
         log format = %o %f %l\n\
         \n\
         [alpha]\n\
         path = /srv/alpha\n",
    );
    let config = RsyncdConfig::from_file(file.path()).expect("parse succeeds");

    assert_eq!(config.global().port(), 9999);
    assert_eq!(config.global().address(), Some("10.0.0.1"));
    assert_eq!(
        config.global().motd_file(),
        Some(Path::new("/etc/motd.txt"))
    );
    assert_eq!(
        config.global().log_file(),
        Some(Path::new("/var/log/daemon.log"))
    );
    assert_eq!(
        config.global().pid_file(),
        Some(Path::new("/var/run/daemon.pid"))
    );
    assert_eq!(config.global().socket_options(), Some("SO_KEEPALIVE"));
    assert_eq!(config.global().log_format(), Some("%o %f %l"));
    assert_eq!(config.modules().len(), 1);
    assert_eq!(config.modules()[0].name(), "alpha");
}

#[test]
fn module_directives_override_defaults() {
    let file = write_config(
        "[override]\n\
         path = /data/override\n\
         read only = no\n\
         write only = yes\n\
         list = no\n\
         use chroot = no\n\
         numeric ids = yes\n\
         fake super = yes\n\
         transfer logging = yes\n\
         timeout = 600\n\
         max connections = 50\n",
    );
    let config = RsyncdConfig::from_file(file.path()).expect("parse succeeds");
    let module = &config.modules()[0];

    assert!(!module.read_only());
    assert!(module.write_only());
    assert!(!module.list());
    assert!(!module.use_chroot());
    assert!(module.numeric_ids());
    assert!(module.fake_super());
    assert!(module.transfer_logging());
    assert_eq!(module.timeout(), Some(600));
    assert_eq!(module.max_connections(), 50);
}

#[test]
fn directive_in_one_module_does_not_affect_another() {
    let file = write_config(
        "[secure]\n\
         path = /srv/secure\n\
         read only = no\n\
         use chroot = no\n\
         numeric ids = yes\n\
         fake super = yes\n\
         timeout = 120\n\
         \n\
         [public]\n\
         path = /srv/public\n",
    );
    let config = RsyncdConfig::from_file(file.path()).expect("parse succeeds");
    assert_eq!(config.modules().len(), 2);

    let secure = config.get_module("secure").unwrap();
    assert!(!secure.read_only());
    assert!(!secure.use_chroot());
    assert!(secure.numeric_ids());
    assert!(secure.fake_super());
    assert_eq!(secure.timeout(), Some(120));

    let public = config.get_module("public").unwrap();
    assert!(public.read_only());
    assert!(public.use_chroot());
    assert!(!public.numeric_ids());
    assert!(!public.fake_super());
    assert!(public.timeout().is_none());
}

#[test]
fn global_directive_after_module_section_is_treated_as_module_directive() {
    // In upstream rsync, once a [module] section starts, all subsequent directives
    // belong to that module. A "port" directive inside a module is an unknown
    // module directive, silently ignored. It does NOT update the global port.
    let file = write_config(
        "port = 8873\n\
         \n\
         [mymod]\n\
         path = /data\n\
         port = 9999\n",
    );
    let config = RsyncdConfig::from_file(file.path()).expect("parse succeeds");

    assert_eq!(config.global().port(), 8873);
    assert_eq!(config.modules().len(), 1);
    assert_eq!(config.modules()[0].name(), "mymod");
    assert_eq!(config.modules()[0].path(), Path::new("/data"));
}

#[test]
fn global_only_directives_silently_ignored_inside_module() {
    // Global-only directives like "address", "pid file", "log file" that appear
    // inside a module section are treated as unknown module directives and ignored.
    let file = write_config(
        "[mod]\n\
         path = /data\n\
         address = 10.0.0.1\n\
         pid file = /var/run/test.pid\n\
         log file = /var/log/test.log\n\
         socket options = SO_KEEPALIVE\n",
    );
    let config = RsyncdConfig::from_file(file.path()).expect("parse succeeds");

    assert!(config.global().address().is_none());
    assert!(config.global().pid_file().is_none());
    assert!(config.global().log_file().is_none());
    assert!(config.global().socket_options().is_none());

    assert_eq!(config.modules().len(), 1);
    assert_eq!(config.modules()[0].path(), Path::new("/data"));
}

#[test]
fn multiple_modules_each_have_independent_settings() {
    let file = write_config(
        "[alpha]\n\
         path = /srv/alpha\n\
         read only = no\n\
         timeout = 60\n\
         max connections = 5\n\
         \n\
         [beta]\n\
         path = /srv/beta\n\
         use chroot = no\n\
         numeric ids = yes\n\
         \n\
         [gamma]\n\
         path = /srv/gamma\n\
         fake super = yes\n\
         transfer logging = yes\n",
    );
    let config = RsyncdConfig::from_file(file.path()).expect("parse succeeds");
    assert_eq!(config.modules().len(), 3);

    let alpha = config.get_module("alpha").unwrap();
    assert!(!alpha.read_only());
    assert_eq!(alpha.timeout(), Some(60));
    assert_eq!(alpha.max_connections(), 5);
    assert!(alpha.use_chroot());
    assert!(!alpha.numeric_ids());
    assert!(!alpha.fake_super());
    assert!(!alpha.transfer_logging());

    let beta = config.get_module("beta").unwrap();
    assert!(beta.read_only());
    assert!(beta.timeout().is_none());
    assert_eq!(beta.max_connections(), 0);
    assert!(!beta.use_chroot());
    assert!(beta.numeric_ids());
    assert!(!beta.fake_super());
    assert!(!beta.transfer_logging());

    let gamma = config.get_module("gamma").unwrap();
    assert!(gamma.read_only());
    assert!(gamma.timeout().is_none());
    assert_eq!(gamma.max_connections(), 0);
    assert!(gamma.use_chroot());
    assert!(!gamma.numeric_ids());
    assert!(gamma.fake_super());
    assert!(gamma.transfer_logging());
}

#[test]
fn parse_syslog_facility_default() {
    let file = write_config("");
    let config = RsyncdConfig::from_file(file.path()).expect("parse succeeds");
    assert_eq!(config.global().syslog_facility(), "daemon");
}

#[test]
fn parse_syslog_facility_custom() {
    let file = write_config("syslog facility = local5\n");
    let config = RsyncdConfig::from_file(file.path()).expect("parse succeeds");
    assert_eq!(config.global().syslog_facility(), "local5");
}

#[test]
fn parse_syslog_facility_auth() {
    let file = write_config("syslog facility = auth\n");
    let config = RsyncdConfig::from_file(file.path()).expect("parse succeeds");
    assert_eq!(config.global().syslog_facility(), "auth");
}

#[test]
fn parse_syslog_tag_default() {
    let file = write_config("");
    let config = RsyncdConfig::from_file(file.path()).expect("parse succeeds");
    assert_eq!(config.global().syslog_tag(), "oc-rsyncd");
}

#[test]
fn parse_syslog_tag_custom() {
    let file = write_config("syslog tag = myapp\n");
    let config = RsyncdConfig::from_file(file.path()).expect("parse succeeds");
    assert_eq!(config.global().syslog_tag(), "myapp");
}

#[test]
fn parse_syslog_tag_upstream_default() {
    let file = write_config("syslog tag = rsyncd\n");
    let config = RsyncdConfig::from_file(file.path()).expect("parse succeeds");
    assert_eq!(config.global().syslog_tag(), "rsyncd");
}

#[test]
fn parse_both_syslog_directives() {
    let file = write_config("syslog facility = local3\nsyslog tag = backup\n");
    let config = RsyncdConfig::from_file(file.path()).expect("parse succeeds");
    assert_eq!(config.global().syslog_facility(), "local3");
    assert_eq!(config.global().syslog_tag(), "backup");
}

#[test]
fn syslog_directives_inside_module_are_ignored() {
    let file = write_config(
        "[mod]\n\
         path = /data\n\
         syslog facility = local7\n\
         syslog tag = custom\n",
    );
    let config = RsyncdConfig::from_file(file.path()).expect("parse succeeds");
    assert_eq!(config.global().syslog_facility(), "daemon");
    assert_eq!(config.global().syslog_tag(), "oc-rsyncd");
    assert_eq!(config.modules().len(), 1);
}

#[test]
fn global_and_multiple_modules_with_mixed_settings() {
    let file = write_config(
        "port = 8873\n\
         log file = /var/log/rsyncd.log\n\
         \n\
         [readonly]\n\
         path = /srv/readonly\n\
         read only = yes\n\
         \n\
         [writable]\n\
         path = /srv/writable\n\
         read only = no\n\
         write only = no\n\
         \n\
         [hidden]\n\
         path = /srv/hidden\n\
         list = no\n",
    );
    let config = RsyncdConfig::from_file(file.path()).expect("parse succeeds");

    assert_eq!(config.global().port(), 8873);
    assert_eq!(
        config.global().log_file(),
        Some(Path::new("/var/log/rsyncd.log"))
    );
    assert_eq!(config.modules().len(), 3);

    let readonly = config.get_module("readonly").unwrap();
    assert!(readonly.read_only());
    assert!(readonly.list());

    let writable = config.get_module("writable").unwrap();
    assert!(!writable.read_only());
    assert!(!writable.write_only());
    assert!(writable.list());

    let hidden = config.get_module("hidden").unwrap();
    assert!(hidden.read_only());
    assert!(!hidden.list());
}

#[test]
fn syslog_facility_parsed_from_global() {
    let file = write_config("syslog facility = local5\n[m]\npath = /srv/m\n");
    let config = RsyncdConfig::from_file(file.path()).expect("parse succeeds");
    assert_eq!(config.global().syslog_facility(), "local5");
}

#[test]
fn syslog_tag_parsed_from_global() {
    let file = write_config("syslog tag = my-daemon\n[m]\npath = /srv/m\n");
    let config = RsyncdConfig::from_file(file.path()).expect("parse succeeds");
    assert_eq!(config.global().syslog_tag(), "my-daemon");
}

#[test]
fn syslog_defaults_when_not_configured() {
    let file = write_config("[m]\npath = /srv/m\n");
    let config = RsyncdConfig::from_file(file.path()).expect("parse succeeds");
    assert_eq!(config.global().syslog_facility(), "daemon");
    assert_eq!(config.global().syslog_tag(), "oc-rsyncd");
}

#[test]
fn syslog_facility_and_tag_both_parsed() {
    let file = write_config(
        "syslog facility = auth\n\
         syslog tag = backup-daemon\n\
         [m]\npath = /srv/m\n",
    );
    let config = RsyncdConfig::from_file(file.path()).expect("parse succeeds");
    assert_eq!(config.global().syslog_facility(), "auth");
    assert_eq!(config.global().syslog_tag(), "backup-daemon");
}

#[test]
fn proxy_protocol_true() {
    let file = write_config("proxy protocol = yes\n[m]\npath = /srv/m\n");
    let config = RsyncdConfig::from_file(file.path()).expect("parse succeeds");
    assert!(config.global().proxy_protocol());
}

#[test]
fn proxy_protocol_false() {
    let file = write_config("proxy protocol = no\n[m]\npath = /srv/m\n");
    let config = RsyncdConfig::from_file(file.path()).expect("parse succeeds");
    assert!(!config.global().proxy_protocol());
}

#[test]
fn proxy_protocol_default_is_false() {
    let file = write_config("[m]\npath = /srv/m\n");
    let config = RsyncdConfig::from_file(file.path()).expect("parse succeeds");
    assert!(!config.global().proxy_protocol());
}

#[test]
fn proxy_protocol_invalid_value_errors() {
    let file = write_config("proxy protocol = maybe\n[m]\npath = /srv/m\n");
    let result = RsyncdConfig::from_file(file.path());
    assert!(result.is_err());
    assert!(format!("{}", result.unwrap_err()).contains("invalid boolean value"));
}

#[test]
fn daemon_chroot_parsed() {
    let file = write_config("daemon chroot = /var/chroot\n[m]\npath = /srv/m\n");
    let config = RsyncdConfig::from_file(file.path()).unwrap();
    assert_eq!(
        config.global().daemon_chroot(),
        Some(Path::new("/var/chroot"))
    );
}

#[test]
fn daemon_chroot_default_is_none() {
    let file = write_config("[m]\npath = /srv/m\n");
    let config = RsyncdConfig::from_file(file.path()).unwrap();
    assert!(config.global().daemon_chroot().is_none());
}

#[test]
fn daemon_chroot_empty_value_errors() {
    let file = write_config("daemon chroot = \n[m]\npath = /srv/m\n");
    let result = RsyncdConfig::from_file(file.path());
    assert!(result.is_err());
    assert!(format!("{}", result.unwrap_err()).contains("must not be empty"));
}

/// Regression test for GitHub issue #1927.
///
/// Asserts that a `fake super = yes` directive inside a daemon module
/// section is parsed into `ModuleConfig::fake_super() == true`. Upstream
/// rsync treats this directive as the trigger that demotes the receiver's
/// `am_root` state to the fake-super marker (`am_root = -1` in
/// `clientserver.c:1080-1107`), so that ownership and special-file
/// metadata are encoded into the `user.rsync.%stat` xattr instead of
/// being applied via real-root syscalls and `mknod` is gated off.
///
/// Pending #1903: extend this test once `load_fake_super` wiring lands
/// in the receiver path so it can also assert the deeper invariant
/// (effective `am_root == false` on the receiver when the module's
/// `fake_super` flag is set, regardless of the running EUID). The
/// receiver-side wiring and the matching `am_root()` tri-state demotion
/// are tracked in `docs/audits/fake-super-privilege.md` (F3, F6); both
/// must be in place before that assertion is meaningful.
#[test]
fn module_fake_super_yes_parses_to_true_for_am_root_demotion() {
    let file = write_config(
        "[backups]\n\
         path = /srv/backups\n\
         fake super = yes\n",
    );
    let config = RsyncdConfig::from_file(file.path()).expect("parse succeeds");

    let module = config
        .get_module("backups")
        .expect("backups module is present");

    // Parsing-layer contract: the module records that fake-super is enabled.
    // This is the input the daemon's connection setup must read in order to
    // demote the receiver's am_root state, per upstream clientserver.c.
    assert!(
        module.fake_super(),
        "module 'backups' must report fake_super() == true after parsing 'fake super = yes'; \
         this is the upstream signal (clientserver.c:1080-1107) that demotes the receiver's \
         am_root to the fake-super marker"
    );

    // Default-off baseline: a sibling module without the directive must not
    // inherit fake-super, otherwise the demotion would leak across modules.
    let default_file = write_config("[plain]\npath = /srv/plain\n");
    let default_config = RsyncdConfig::from_file(default_file.path()).expect("parse succeeds");
    let plain = default_config
        .get_module("plain")
        .expect("plain module is present");
    assert!(
        !plain.fake_super(),
        "module 'plain' must default to fake_super() == false when the directive is absent"
    );
}

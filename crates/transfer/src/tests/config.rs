use std::ffi::OsString;
use std::time::{Duration, SystemTime};

use crate::{ServerConfig, ServerRole};

#[test]
fn config_rejects_empty_flag_string() {
    let result =
        ServerConfig::from_flag_string_and_args(ServerRole::Receiver, String::new(), Vec::new());

    assert!(result.is_err());
}

#[test]
fn config_captures_fields() {
    let args = vec![OsString::from("."), OsString::from("dest")];
    let config = ServerConfig::from_flag_string_and_args(
        ServerRole::Generator,
        "-logDtpre.iLsfxC".to_owned(),
        args.clone(),
    )
    .expect("config parses");

    assert_eq!(config.role, ServerRole::Generator);
    assert_eq!(config.flag_string, "-logDtpre.iLsfxC");
    assert_eq!(config.args, args);
}

#[test]
fn config_accepts_empty_flag_string_with_args() {
    // Daemon mode uses empty flag string with module path as argument
    let args = vec![OsString::from("/var/lib/rsync/module")];
    let config =
        ServerConfig::from_flag_string_and_args(ServerRole::Receiver, String::new(), args.clone())
            .expect("config parses with empty flag string and args");

    assert_eq!(config.role, ServerRole::Receiver);
    assert_eq!(config.flag_string, "");
    assert_eq!(config.args, args);
}

#[test]
fn config_receiver_role_with_module_path() {
    // Daemon receiver role (client pushing to daemon)
    let module_path = OsString::from("/srv/rsync/uploads");
    let config = ServerConfig::from_flag_string_and_args(
        ServerRole::Receiver,
        String::new(),
        vec![module_path.clone()],
    )
    .expect("receiver config with module path");

    assert_eq!(config.role, ServerRole::Receiver);
    assert_eq!(config.args.len(), 1);
    assert_eq!(config.args[0], module_path);
}

#[test]
fn config_generator_role_with_module_path() {
    // Daemon generator role (client pulling from read-only daemon)
    let module_path = OsString::from("/srv/rsync/mirror");
    let config = ServerConfig::from_flag_string_and_args(
        ServerRole::Generator,
        String::new(),
        vec![module_path.clone()],
    )
    .expect("generator config with module path");

    assert_eq!(config.role, ServerRole::Generator);
    assert_eq!(config.args.len(), 1);
    assert_eq!(config.args[0], module_path);
}

#[test]
fn config_preserves_role_for_daemon_transfers() {
    // Verify role is correctly set based on module configuration
    let receiver = ServerConfig::from_flag_string_and_args(
        ServerRole::Receiver,
        String::new(),
        vec![OsString::from("/path")],
    )
    .expect("receiver config");

    let generator = ServerConfig::from_flag_string_and_args(
        ServerRole::Generator,
        String::new(),
        vec![OsString::from("/path")],
    )
    .expect("generator config");

    assert_eq!(receiver.role, ServerRole::Receiver);
    assert_eq!(generator.role, ServerRole::Generator);
}

#[test]
fn config_stop_at_default_is_none() {
    let config = ServerConfig::from_flag_string_and_args(
        ServerRole::Receiver,
        String::new(),
        vec![OsString::from("/path")],
    )
    .expect("config parses");
    assert!(config.stop_at.is_none());
}

#[test]
fn config_stop_at_can_be_set() {
    let deadline = SystemTime::now() + Duration::from_secs(3600);
    let mut config = ServerConfig::from_flag_string_and_args(
        ServerRole::Receiver,
        String::new(),
        vec![OsString::from("/path")],
    )
    .expect("config parses");
    config.stop_at = Some(deadline);
    assert!(config.stop_at.is_some());
}

#[test]
fn config_stop_at_survives_clone() {
    let deadline = SystemTime::now() + Duration::from_secs(60);
    let mut config = ServerConfig::from_flag_string_and_args(
        ServerRole::Generator,
        String::new(),
        vec![OsString::from("/path")],
    )
    .expect("config parses");
    config.stop_at = Some(deadline);
    let cloned = config.clone();
    assert_eq!(cloned.stop_at, config.stop_at);
}
#[test]
fn size_limits_default_to_none() {
    let cfg = ServerConfig::from_flag_string_and_args(
        ServerRole::Receiver,
        String::new(),
        vec![OsString::from("/p")],
    )
    .expect("ok");
    let mfs = cfg.min_file_size;
    assert!(mfs.is_none());
    let mxs = cfg.max_file_size;
    assert!(mxs.is_none());
}

#[test]
fn size_limits_can_be_configured() {
    let mut cfg = ServerConfig::from_flag_string_and_args(
        ServerRole::Receiver,
        String::new(),
        vec![OsString::from("/p")],
    )
    .expect("ok");
    cfg.min_file_size = Some(100);
    cfg.max_file_size = Some(1000);
    assert_eq!(cfg.min_file_size, Some(100));
    assert_eq!(cfg.max_file_size, Some(1000));
}

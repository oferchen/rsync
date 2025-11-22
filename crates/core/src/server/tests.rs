use std::ffi::OsString;

use super::{ServerConfig, ServerRole};

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
        "-logDtpre.iLsfxC".to_string(),
        args.clone(),
    )
    .expect("config parses");

    assert_eq!(config.role, ServerRole::Generator);
    assert_eq!(config.flag_string, "-logDtpre.iLsfxC");
    assert_eq!(config.args, args);
}

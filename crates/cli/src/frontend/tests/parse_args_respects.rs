use super::common::*;
use super::*;

#[test]
fn parse_args_respects_env_protect_args_disabled() {
    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _guard = EnvGuard::set("RSYNC_PROTECT_ARGS", OsStr::new("0"));

    let parsed = parse_args([OsString::from(RSYNC)]).expect("parse");

    assert_eq!(parsed.protect_args, Some(false));
}

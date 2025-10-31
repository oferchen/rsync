use super::common::*;
use super::*;

#[test]
fn parse_args_reads_env_protect_args_default() {
    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _guard = EnvGuard::set("RSYNC_PROTECT_ARGS", OsStr::new("1"));

    let parsed = parse_args([OsString::from(RSYNC)]).expect("parse");

    assert_eq!(parsed.protect_args, Some(true));
}

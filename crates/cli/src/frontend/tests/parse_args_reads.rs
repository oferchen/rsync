use super::common::*;
use super::*;

#[test]
fn parse_args_reads_env_protect_args_default() {
    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _guard = EnvGuard::set("RSYNC_PROTECT_ARGS", OsStr::new("1"));

    let parsed = parse_args([OsString::from(RSYNC)]).expect("parse");

    assert_eq!(parsed.protect_args, Some(true));
}

#[test]
fn parse_args_reads_env_rsync_rsh_as_remote_shell() {
    // #7123: RSYNC_RSH in the environment is an IMPLICIT remote shell - the user
    // never typed --rsh/-e. It must populate remote_shell exactly as an explicit
    // -e would (entry.rs), which is why the ssh://×remote-shell diagnostic now
    // names RSYNC_RSH: the conflict can be triggered without any typed option.
    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _guard = EnvGuard::set("RSYNC_RSH", OsStr::new("ssh"));

    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("ssh://host/path"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.remote_shell.as_deref(), Some(OsStr::new("ssh")));
}

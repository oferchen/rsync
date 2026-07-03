//! Local-shell stub: a std-only Rust port of upstream `support/lsh.sh`.
//!
//! Upstream's `lsh.sh` is a "remote shell" that only pretends to connect to
//! `localhost` (or `lh`), running the remote command locally. oc-rsync test
//! legs point `--rsh` / `RSYNC_RSH` at this binary to drive remote-shell code
//! paths without a real SSH server.
//!
//! Argv handling mirrors `lsh.sh` exactly:
//!
//! - `-l USER` / `-lUSER`: run the command via `sudo -H -u USER sh -c ...`.
//! - `--no-cd`: do not change directory before running.
//! - any other `-*`: silently dropped (ssh-style flags oc-rsync may pass).
//! - `localhost`: connect, default cd to `$HOME` (like ssh).
//! - `lh`: connect, imply `--no-cd`.
//! - anything else: print an error to stderr and exit 1.
//!
//! Everything after the accepted host token is the remote argv, run in-process
//! as a child with stdio inherited so oc-rsync's protocol streams pass through
//! transparently.
//!
//! Unix-only: it depends on `sh -c` and POSIX process semantics. The build is
//! gated so Windows does not attempt to compile it.

#[cfg(unix)]
fn main() {
    std::process::exit(run());
}

#[cfg(unix)]
fn run() -> i32 {
    use std::process::Command;

    let mut args = std::env::args().skip(1).peekable();

    let mut user: Option<String> = None;
    // Default matches upstream: cd to the user's home unless the host is "lh"
    // or an explicit --no-cd was passed.
    let mut do_cd = true;
    let mut connected = false;

    while let Some(arg) = args.next() {
        if arg == "-l" {
            user = args.next();
        } else if let Some(u) = arg.strip_prefix("-l") {
            user = Some(u.to_string());
        } else if arg == "--no-cd" {
            do_cd = false;
        } else if arg.starts_with('-') {
            // ssh-style flag oc-rsync may emit; upstream drops it.
        } else if arg == "localhost" {
            connected = true;
            break;
        } else if arg == "lh" {
            do_cd = false;
            connected = true;
            break;
        } else {
            eprintln!("lsh-stub: unable to connect to host {arg}");
            return 1;
        }
    }

    if !connected {
        eprintln!("lsh-stub: no host specified");
        return 1;
    }

    // The remainder is the remote command. Upstream joins it with spaces and
    // hands it to `sh -c` / `eval`, so a single joined string reproduces the
    // same word-splitting behaviour oc-rsync's callers expect.
    let remote: Vec<String> = args.collect();
    if remote.is_empty() {
        eprintln!("lsh-stub: empty remote command");
        return 1;
    }
    let joined = remote.join(" ");

    let mut cmd = if let Some(user) = user {
        // Mirror `sudo -H -u USER sh -c "cd '$HOME' && CMD"`.
        let script = if do_cd {
            match std::env::var("HOME") {
                Ok(home) => format!("cd '{home}' && {joined}"),
                Err(_) => joined.clone(),
            }
        } else {
            joined.clone()
        };
        let mut c = Command::new("sudo");
        c.args(["-H", "-u", &user, "sh", "-c", &script]);
        c
    } else {
        let mut c = Command::new("sh");
        c.arg("-c").arg(&joined);
        if do_cd {
            if let Ok(home) = std::env::var("HOME") {
                c.current_dir(home);
            }
        }
        c
    };

    match cmd.status() {
        Ok(status) => status.code().unwrap_or(1),
        Err(e) => {
            eprintln!("lsh-stub: failed to exec remote command: {e}");
            1
        }
    }
}

#[cfg(not(unix))]
fn main() {
    eprintln!("lsh-stub is Unix-only");
    std::process::exit(1);
}

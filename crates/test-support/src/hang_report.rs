//! Turns a hung child process tree into a failure that says where it hung.
//!
//! A transfer that deadlocks blocks forever, and the only bound on it is the
//! test runner's own: nextest sends `SIGTERM` once a test passes its
//! `slow-timeout` budget. [`run_reporting_hangs`] waits on the child with no
//! clock of its own and hooks that `SIGTERM`. Before the test exits it writes,
//! for every process in the child's process group, each thread's name, state
//! and kernel wait channel, plus a user-space backtrace when `eu-stack` is
//! allowed to attach, and then kills the group so no transfer outlives the test.

use std::io;
use std::process::{Command, Output, Stdio};
use std::sync::{Mutex, Once};

/// Process group of the child currently being waited on, if any.
static WATCHED_GROUP: Mutex<Option<u32>> = Mutex::new(None);
static HOOK: Once = Once::new();

/// Runs `command` to completion in its own process group.
///
/// Stdin is null and both output streams are captured, as with
/// [`Command::output`]. The child gets a fresh process group, so the
/// runner's `SIGTERM` reaches this process and not the transfer, which keeps
/// the hung state intact for the report.
///
/// # Errors
///
/// Returns the error from installing the `SIGTERM` hook, spawning the child or
/// collecting its output.
pub fn run_reporting_hangs(command: &mut Command) -> io::Result<Output> {
    use std::os::unix::process::CommandExt;

    install_hook()?;
    let child = command
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    set_watched(Some(child.id()));
    let output = child.wait_with_output();
    set_watched(None);
    output
}

fn set_watched(group: Option<u32>) {
    *WATCHED_GROUP.lock().unwrap_or_else(|e| e.into_inner()) = group;
}

fn install_hook() -> io::Result<()> {
    let mut result = Ok(());
    HOOK.call_once(|| result = platform::signal::on_sigterm(report_and_exit));
    result
}

/// The `SIGTERM` path: report every process of the watched group, kill the
/// group, and fail the test.
fn report_and_exit() {
    let group = *WATCHED_GROUP.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(group) = group {
        eprintln!("hang: terminated while waiting on process group {group}");
        for pid in group_members(group) {
            eprintln!("{}", describe_process(pid));
        }
        // `kill -KILL -- -PGID` signals the whole group.
        let _ = Command::new("kill")
            .args(["-KILL", "--", &format!("-{group}")])
            .status();
    }
    std::process::exit(1);
}

/// Every process whose process group is `group`.
#[cfg(target_os = "linux")]
fn group_members(group: u32) -> Vec<u32> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    let mut pids: Vec<u32> = entries
        .filter_map(|e| e.ok()?.file_name().to_str()?.parse().ok())
        .filter(|pid| stat_fields(&format!("/proc/{pid}/stat")).is_some_and(|f| f.pgrp == group))
        .collect();
    pids.sort_unstable();
    pids
}

#[cfg(not(target_os = "linux"))]
fn group_members(group: u32) -> Vec<u32> {
    vec![group]
}

/// The fields of a `/proc/.../stat` line the report uses.
#[cfg(target_os = "linux")]
struct StatFields {
    state: char,
    pgrp: u32,
}

/// Parses `/proc/.../stat`. The command name is parenthesised and may contain
/// spaces, so fields are counted from the last `)`.
#[cfg(target_os = "linux")]
fn stat_fields(path: &str) -> Option<StatFields> {
    let line = std::fs::read_to_string(path).ok()?;
    let mut rest = line.get(line.rfind(')')? + 1..)?.split_whitespace();
    let state = rest.next()?.chars().next()?;
    let _ppid = rest.next()?;
    let pgrp = rest.next()?.parse().ok()?;
    Some(StatFields { state, pgrp })
}

/// One process: its command line, one line per thread, and a backtrace when
/// one can be taken.
#[cfg(target_os = "linux")]
fn describe_process(pid: u32) -> String {
    let read = |p: String| std::fs::read_to_string(p).unwrap_or_default();
    let cmdline = read(format!("/proc/{pid}/cmdline")).replace('\0', " ");
    let mut out = format!("process {pid}: {}", cmdline.trim_end());
    if let Ok(tasks) = std::fs::read_dir(format!("/proc/{pid}/task")) {
        let mut tids: Vec<String> = tasks
            .filter_map(|t| t.ok()?.file_name().into_string().ok())
            .collect();
        tids.sort();
        for tid in tids {
            let task = format!("/proc/{pid}/task/{tid}");
            let state = stat_fields(&format!("{task}/stat")).map_or('?', |f| f.state);
            out.push_str(&format!(
                "\n  thread {tid} {:<16} state {state} wchan {}",
                read(format!("{task}/comm")).trim_end(),
                read(format!("{task}/wchan")).trim_end(),
            ));
        }
    }
    match Command::new("eu-stack")
        .args(["-p", &pid.to_string()])
        .output()
    {
        Ok(stack) if stack.status.success() => {
            out.push('\n');
            out.push_str(&String::from_utf8_lossy(&stack.stdout));
        }
        _ => out.push_str("\n  (no user-space backtrace: eu-stack missing or not permitted)"),
    }
    out
}

#[cfg(not(target_os = "linux"))]
fn describe_process(pid: u32) -> String {
    format!("process group {pid}: per-thread state needs /proc")
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use std::io::{BufRead, BufReader};
    use std::os::unix::process::CommandExt;

    use super::*;

    /// The report reads the command line, state and group of a live process,
    /// so a hang is described rather than just killed.
    #[test]
    fn the_report_describes_a_live_process_group() {
        // `/proc/<pid>/cmdline` is stable only once the child is past its
        // last exec: before it the entry is this test's forked copy, and
        // during it the command line is empty. The shell therefore announces
        // itself after its own exec and then blocks in the `read` builtin on
        // a stdin this test holds open, so it neither execs again nor forks.
        let mut blocker = Command::new("sh")
            .args(["-c", "echo ready; read hold"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .process_group(0)
            .spawn()
            .expect("spawn sh");
        let mut ready = String::new();
        BufReader::new(blocker.stdout.take().expect("piped stdout"))
            .read_line(&mut ready)
            .expect("read the readiness line");
        assert_eq!(ready, "ready\n");
        let group = blocker.id();
        assert_eq!(group_members(group), vec![group]);
        let report = describe_process(group);
        assert!(report.contains("-c echo ready; read hold"), "{report}");
        // The shell may still be on its way into `read`, so any state the
        // kernel reports is right; `?` is the placeholder for an unreadable one.
        assert!(report.contains(&format!("\n  thread {group} ")), "{report}");
        assert!(!report.contains(" state ? "), "{report}");
        blocker.kill().expect("kill sh");
        blocker.wait().expect("reap sh");
    }
}

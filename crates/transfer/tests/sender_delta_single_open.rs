//! One source open per file on the sender's delta path.
//!
//! upstream: `sender.c:656-800` - `send_files()` opens each source once and
//! reads the token scan and the whole-file checksum through `map_file()` over
//! that fd. oc used to open the source a second time to re-read matched
//! blocks. A daemon push makes the client the sender while the daemon runs as
//! a separate, untraced process, so every open in the client's trace is the
//! sender's.

#![cfg(target_os = "linux")]

use std::ffi::OsStr;
use std::fs;
use std::path::Path;

use test_support::syscall_trace::OpenTrace;
use test_support::{DaemonProcess, locate_command_on_path, oc_rsync_bin, test_name};

const FILES: usize = 6;
const FILE_LEN: usize = 64 * 1024;

fn lcg_bytes(seed: u64, len: usize) -> Vec<u8> {
    let mut state = seed;
    (0..len)
        .map(|_| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (state >> 33) as u8
        })
        .collect()
}

fn write_config(config: &Path, module_root: &Path) {
    let body = format!(
        "use chroot = false\n\
         log file = {log}\n\
         \n\
         [dst]\n\
         path = {root}\n\
         read only = false\n",
        log = config.with_file_name("rsyncd.log").display(),
        root = module_root.display(),
    );
    fs::write(config, body).expect("write rsyncd.conf");
}

/// Parses the `Matched data: N bytes` figure from `--stats` output.
fn matched_data(stats: &str) -> u64 {
    stats
        .lines()
        .find_map(|line| line.strip_prefix("Matched data: "))
        .and_then(|rest| rest.split_whitespace().next())
        .map(|n| n.replace(',', "").parse().expect("matched data figure"))
        .expect("--stats prints Matched data")
}

// WHY: a second open of the source re-resolves its path, so a rename between
// the opens mixes two inodes into one token stream; upstream opens once. The
// basis shares most blocks with the source, so the delta has matched blocks -
// the only case in which the old code reopened the file - and the stats
// check proves they were there.
#[test]
fn a_delta_push_opens_each_source_file_once() {
    let Some(strace) = locate_command_on_path("strace") else {
        eprintln!("SKIP {}: strace not on PATH", test_name!());
        return;
    };
    let dir = tempfile::tempdir().expect("tempdir");
    let (src, module) = (dir.path().join("src"), dir.path().join("module"));
    fs::create_dir_all(&src).expect("src dir");
    fs::create_dir_all(&module).expect("module dir");
    let names: Vec<String> = (0..FILES).map(|i| format!("file{i}.bin")).collect();
    for (seed, name) in names.iter().enumerate() {
        let content = lcg_bytes(seed as u64 + 1, FILE_LEN);
        // A changed middle and a shorter tail: matched blocks around a literal
        // run, and a size the quick check cannot skip.
        let mut basis = content[..FILE_LEN - 1000].to_vec();
        basis[FILE_LEN / 2..FILE_LEN / 2 + 4096].fill(0);
        fs::write(src.join(name), &content).expect("source file");
        fs::write(module.join(name), &basis).expect("basis file");
    }
    let config = dir.path().join("rsyncd.conf");
    write_config(&config, &module);
    let daemon = DaemonProcess::spawn(&config, std::iter::empty::<&str>()).expect("daemon");

    let (output, trace) = OpenTrace::record(
        &strace,
        &dir.path().join("opens.trace"),
        &oc_rsync_bin(),
        [
            "-a".to_owned(),
            "--no-whole-file".to_owned(),
            "--stats".to_owned(),
            format!("{}/", src.display()),
            format!("rsync://127.0.0.1:{}/dst/", daemon.port()),
        ],
    )
    .expect("traced push");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "push failed: {}\nstdout:\n{stdout}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        matched_data(&stdout) > 0,
        "no matched blocks, so the delta path was not exercised:\n{stdout}"
    );
    let opens: Vec<(&str, usize)> = names
        .iter()
        .map(|name| (name.as_str(), trace.opens_of(OsStr::new(name))))
        .collect();
    let once: Vec<(&str, usize)> = names.iter().map(|name| (name.as_str(), 1)).collect();
    assert_eq!(
        opens, once,
        "the sender must open each source file exactly once"
    );
    for name in &names {
        assert_eq!(
            fs::read(module.join(name)).expect("pushed file"),
            fs::read(src.join(name)).expect("source file"),
            "{name} reconstructed differently"
        );
    }
}

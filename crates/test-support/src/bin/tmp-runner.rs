//! Cargo target runner giving each test process a private temp directory.
//!
//! Invoked as `tmp-runner <test-binary> [args...]`; see
//! `test_support::tmp_runner` for why and how.

use test_support::tmp_runner::{RUNNER_SETUP_FAILURE, run};

fn main() {
    let mut args = std::env::args_os().skip(1);
    let Some(program) = args.next() else {
        eprintln!("usage: tmp-runner <program> [args...]");
        std::process::exit(RUNNER_SETUP_FAILURE);
    };
    std::process::exit(run(program, args));
}

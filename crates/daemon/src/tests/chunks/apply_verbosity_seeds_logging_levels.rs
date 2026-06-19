/// Verifies that the daemon's `-v` counter seeds the thread-local
/// [`logging::VerbosityConfig`] so subsequent `info_gte` / `debug_gte`
/// checks gate log output per upstream's `set_output_verbosity()` semantics.
///
/// Before this wire-up landed (PR #5887 follow-up), the daemon stored the
/// `-v` count in `RuntimeOptions.verbosity` but never translated it into
/// the logging crate's flag-level state, so any `info_gte(NAME, 2)` /
/// `debug_gte(CMD, 1)` check anywhere in the protocol or transfer crates
/// short-circuited at the default of 0 regardless of how many `-v` flags
/// the operator stacked. Result: daemons were diagnostically silent under
/// `-vv` / `-vvv`, breaking upstream parity and surfacing as a UTS test
/// divergence.
///
/// Each subtest spawns a fresh thread because [`logging::init`] writes to
/// thread-local storage; running everything in the test thread would
/// pollute other tests sharing the harness's main thread.
#[test]
fn apply_verbosity_seeds_logging_levels() {
    use logging::{DebugFlag, InfoFlag};

    // verbosity = 0 -> only the default `nonreg` flag is set; nothing else
    // crosses the gate. Mirrors upstream's `info_verbosity[0] = "NONREG"`
    // table entry in `options.c:228`.
    std::thread::spawn(|| {
        apply_verbosity(0);
        assert!(logging::info_gte(InfoFlag::Nonreg, 1));
        assert!(!logging::info_gte(InfoFlag::Name, 1));
        assert!(!logging::info_gte(InfoFlag::Copy, 1));
        assert!(!logging::debug_gte(DebugFlag::Cmd, 1));
    })
    .join()
    .expect("verbosity 0 thread");

    // verbosity = 1 -> COPY,DEL,FLIST,MISC,NAME,STATS,SYMSAFE info flags
    // are enabled at level 1 (upstream `info_verbosity[1]`).
    std::thread::spawn(|| {
        apply_verbosity(1);
        assert!(logging::info_gte(InfoFlag::Name, 1));
        assert!(logging::info_gte(InfoFlag::Copy, 1));
        assert!(logging::info_gte(InfoFlag::Flist, 1));
        assert!(!logging::info_gte(InfoFlag::Name, 2));
        assert!(!logging::debug_gte(DebugFlag::Cmd, 1));
    })
    .join()
    .expect("verbosity 1 thread");

    // verbosity = 2 -> debug flags BIND,CMD,CONNECT,DEL,DELTASUM,... cross
    // their gates at level 1, and info NAME/MISC bump to level 2 (upstream
    // `debug_verbosity[2]` + `info_verbosity[2]`).
    std::thread::spawn(|| {
        apply_verbosity(2);
        assert!(logging::info_gte(InfoFlag::Name, 2));
        assert!(logging::info_gte(InfoFlag::Misc, 2));
        assert!(logging::debug_gte(DebugFlag::Cmd, 1));
        assert!(logging::debug_gte(DebugFlag::Flist, 1));
        assert!(!logging::debug_gte(DebugFlag::Recv, 1));
    })
    .join()
    .expect("verbosity 2 thread");

    // verbosity = 3 -> RECV/SEND/GENR/OWN debug flags activate at level 1
    // (upstream `debug_verbosity[3]`). This is the cumulative behaviour
    // documented in `set_output_verbosity()` (upstream: options.c:530).
    std::thread::spawn(|| {
        apply_verbosity(3);
        assert!(logging::debug_gte(DebugFlag::Recv, 1));
        assert!(logging::debug_gte(DebugFlag::Send, 1));
        assert!(logging::debug_gte(DebugFlag::Genr, 1));
    })
    .join()
    .expect("verbosity 3 thread");
}

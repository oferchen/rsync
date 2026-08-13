//! Table-driven `--chmod` conformance matrix pinned to upstream rsync.
//!
//! Two grids are evaluated here, because `--chmod` has two defect classes worth
//! guarding as classes rather than as single cases:
//!
//! * [`MATRIX`] - "a who-letter does not expand to every bit it covers":
//!   `--chmod=a+s` set setuid only, because the `a` clause left the set-id top
//!   bits unset and the `s` clause fell through to its no-who default. A single
//!   `a+s` assertion would not catch the siblings of that bug (`a-s`, `a=s`,
//!   `a+st`, `a=t`, ...), so the whole `{who} x {op} x {perm}` grid runs.
//! * [`COPY_MATRIX`] / [`COPY_SEQUENCE_MATRIX`] / [`REJECTED_RHS`] - the
//!   chmod(1)-style permission copies (`u=g`, `g+u`, `o=u`) rsync 3.5.0 added.
//!   Their whole substance is *which* class is read, *which* classes are
//!   written, and *when* the source is sampled, so the grid spans
//!   `{who} x {op} x {source class} x {starting mode}`, plus multi-clause rows
//!   that pin the order dependence and an exhaustive accept/reject sweep that
//!   pins the grammar against widening.
//!
//! upstream: chmod.c `parse_chmod()` + `tweak_mode()`. Every expected value
//! below was produced by compiling rsync 3.5.0's `chmod.c` verbatim against a
//! stubbed `rsync.h` and printing `tweak_mode()` for each spec and probe, so the
//! tables are upstream's own output rather than a hand-derivation.

use super::apply::apply_clauses;
use super::parse::parse_with_umask;

/// Umask folded into clauses that omit the who-class. Fixed so the implied-who
/// rows (`+w`, `+X`, ...) are reproducible regardless of the test process.
/// upstream: chmod.c `parse_chmod()` masks implied-who bits with `~orig_umask`.
const UMASK: u32 = 0o022;

/// Starting modes each spec is evaluated against, in [`MATRIX`] column order.
/// The last probe is a directory; `07777` exercises bit clearing.
const PROBES: [(u32, bool); 4] = [
    (0o644, false),
    (0o755, false),
    (0o7777, false),
    (0o750, true),
];

/// Who-classes every grid is swept over. upstream: chmod.c parse_chmod()
/// STATE_1ST_HALF accepts `u`, `g`, `o`, `a` in any combination, or none.
const WHO: [&str; 8] = ["", "u", "g", "o", "a", "ug", "go", "ugo"];

/// Operators every grid is swept over. upstream: `CHMOD_ADD`, `CHMOD_SUB`,
/// `CHMOD_EQ`.
const OP: [&str; 3] = ["+", "-", "="];

/// Literal permission halves [`MATRIX`] is swept over.
const WHAT: [&str; 12] = [
    "r", "w", "x", "X", "s", "t", "rw", "rwx", "rwxXst", "st", "sX", "ts",
];

/// Permission *classes* a copy may read from. upstream: chmod.c parse_chmod()
/// STATE_2ND_HALF `case 'u'/'g'/'o'` - `a` is deliberately absent.
const COPY_SOURCES: [&str; 3] = ["u", "g", "o"];

/// `(spec, [result per PROBES entry])` as produced by upstream rsync 3.5.0.
const MATRIX: &[(&str, [u32; 4])] = &[
    ("+r", [0o0644, 0o0755, 0o7777, 0o0754]),
    ("+w", [0o0644, 0o0755, 0o7777, 0o0750]),
    ("+x", [0o0755, 0o0755, 0o7777, 0o0751]),
    ("+X", [0o0644, 0o0755, 0o7777, 0o0751]),
    ("+s", [0o4644, 0o4755, 0o7777, 0o4750]),
    ("+t", [0o1644, 0o1755, 0o7777, 0o1750]),
    ("+rw", [0o0644, 0o0755, 0o7777, 0o0754]),
    ("+rwx", [0o0755, 0o0755, 0o7777, 0o0755]),
    ("+rwxXst", [0o5644, 0o5755, 0o7777, 0o5755]),
    ("+st", [0o5644, 0o5755, 0o7777, 0o5750]),
    ("+sX", [0o4644, 0o4755, 0o7777, 0o4751]),
    ("+ts", [0o4644, 0o4755, 0o7777, 0o4750]),
    ("-r", [0o0200, 0o0311, 0o7333, 0o0310]),
    ("-w", [0o0444, 0o0555, 0o7577, 0o0550]),
    ("-x", [0o0644, 0o0644, 0o7666, 0o0640]),
    ("-X", [0o0644, 0o0644, 0o7666, 0o0640]),
    ("-s", [0o0644, 0o0755, 0o3777, 0o0750]),
    ("-t", [0o0644, 0o0755, 0o6777, 0o0750]),
    ("-rw", [0o0000, 0o0111, 0o7133, 0o0110]),
    ("-rwx", [0o0000, 0o0000, 0o7022, 0o0000]),
    ("-rwxXst", [0o0000, 0o0000, 0o2022, 0o0000]),
    ("-st", [0o0644, 0o0755, 0o2777, 0o0750]),
    ("-sX", [0o0644, 0o0644, 0o3666, 0o0640]),
    ("-ts", [0o0644, 0o0755, 0o3777, 0o0750]),
    ("=r", [0o0444, 0o0444, 0o7444, 0o0444]),
    ("=w", [0o0200, 0o0200, 0o7200, 0o0200]),
    ("=x", [0o0111, 0o0111, 0o7111, 0o0111]),
    ("=X", [0o0000, 0o0111, 0o7111, 0o0111]),
    ("=s", [0o4000, 0o4000, 0o7000, 0o4000]),
    ("=t", [0o1000, 0o1000, 0o7000, 0o1000]),
    ("=rw", [0o0644, 0o0644, 0o7644, 0o0644]),
    ("=rwx", [0o0755, 0o0755, 0o7755, 0o0755]),
    ("=rwxXst", [0o5644, 0o5755, 0o7755, 0o5755]),
    ("=st", [0o5000, 0o5000, 0o7000, 0o5000]),
    ("=sX", [0o4000, 0o4111, 0o7111, 0o4111]),
    ("=ts", [0o4000, 0o4000, 0o7000, 0o4000]),
    ("u+r", [0o0644, 0o0755, 0o7777, 0o0750]),
    ("u+w", [0o0644, 0o0755, 0o7777, 0o0750]),
    ("u+x", [0o0744, 0o0755, 0o7777, 0o0750]),
    ("u+X", [0o0644, 0o0755, 0o7777, 0o0750]),
    ("u+s", [0o4644, 0o4755, 0o7777, 0o4750]),
    ("u+t", [0o1644, 0o1755, 0o7777, 0o1750]),
    ("u+rw", [0o0644, 0o0755, 0o7777, 0o0750]),
    ("u+rwx", [0o0744, 0o0755, 0o7777, 0o0750]),
    ("u+rwxXst", [0o5644, 0o5755, 0o7777, 0o5750]),
    ("u+st", [0o5644, 0o5755, 0o7777, 0o5750]),
    ("u+sX", [0o4644, 0o4755, 0o7777, 0o4750]),
    ("u+ts", [0o5644, 0o5755, 0o7777, 0o5750]),
    ("u-r", [0o0244, 0o0355, 0o7377, 0o0350]),
    ("u-w", [0o0444, 0o0555, 0o7577, 0o0550]),
    ("u-x", [0o0644, 0o0655, 0o7677, 0o0650]),
    ("u-X", [0o0644, 0o0655, 0o7677, 0o0650]),
    ("u-s", [0o0644, 0o0755, 0o3777, 0o0750]),
    ("u-t", [0o0644, 0o0755, 0o6777, 0o0750]),
    ("u-rw", [0o0044, 0o0155, 0o7177, 0o0150]),
    ("u-rwx", [0o0044, 0o0055, 0o7077, 0o0050]),
    ("u-rwxXst", [0o0044, 0o0055, 0o2077, 0o0050]),
    ("u-st", [0o0644, 0o0755, 0o2777, 0o0750]),
    ("u-sX", [0o0644, 0o0655, 0o3677, 0o0650]),
    ("u-ts", [0o0644, 0o0755, 0o2777, 0o0750]),
    ("u=r", [0o0444, 0o0455, 0o7477, 0o0450]),
    ("u=w", [0o0244, 0o0255, 0o7277, 0o0250]),
    ("u=x", [0o0144, 0o0155, 0o7177, 0o0150]),
    ("u=X", [0o0044, 0o0155, 0o7177, 0o0150]),
    ("u=s", [0o4044, 0o4055, 0o7077, 0o4050]),
    ("u=t", [0o1044, 0o1055, 0o3077, 0o1050]),
    ("u=rw", [0o0644, 0o0655, 0o7677, 0o0650]),
    ("u=rwx", [0o0744, 0o0755, 0o7777, 0o0750]),
    ("u=rwxXst", [0o5644, 0o5755, 0o7777, 0o5750]),
    ("u=st", [0o5044, 0o5055, 0o7077, 0o5050]),
    ("u=sX", [0o4044, 0o4155, 0o7177, 0o4150]),
    ("u=ts", [0o5044, 0o5055, 0o7077, 0o5050]),
    ("g+r", [0o0644, 0o0755, 0o7777, 0o0750]),
    ("g+w", [0o0664, 0o0775, 0o7777, 0o0770]),
    ("g+x", [0o0654, 0o0755, 0o7777, 0o0750]),
    ("g+X", [0o0644, 0o0755, 0o7777, 0o0750]),
    ("g+s", [0o2644, 0o2755, 0o7777, 0o2750]),
    ("g+t", [0o1644, 0o1755, 0o7777, 0o1750]),
    ("g+rw", [0o0664, 0o0775, 0o7777, 0o0770]),
    ("g+rwx", [0o0674, 0o0775, 0o7777, 0o0770]),
    ("g+rwxXst", [0o3664, 0o3775, 0o7777, 0o3770]),
    ("g+st", [0o3644, 0o3755, 0o7777, 0o3750]),
    ("g+sX", [0o2644, 0o2755, 0o7777, 0o2750]),
    ("g+ts", [0o3644, 0o3755, 0o7777, 0o3750]),
    ("g-r", [0o0604, 0o0715, 0o7737, 0o0710]),
    ("g-w", [0o0644, 0o0755, 0o7757, 0o0750]),
    ("g-x", [0o0644, 0o0745, 0o7767, 0o0740]),
    ("g-X", [0o0644, 0o0745, 0o7767, 0o0740]),
    ("g-s", [0o0644, 0o0755, 0o5777, 0o0750]),
    ("g-t", [0o0644, 0o0755, 0o6777, 0o0750]),
    ("g-rw", [0o0604, 0o0715, 0o7717, 0o0710]),
    ("g-rwx", [0o0604, 0o0705, 0o7707, 0o0700]),
    ("g-rwxXst", [0o0604, 0o0705, 0o4707, 0o0700]),
    ("g-st", [0o0644, 0o0755, 0o4777, 0o0750]),
    ("g-sX", [0o0644, 0o0745, 0o5767, 0o0740]),
    ("g-ts", [0o0644, 0o0755, 0o4777, 0o0750]),
    ("g=r", [0o0644, 0o0745, 0o7747, 0o0740]),
    ("g=w", [0o0624, 0o0725, 0o7727, 0o0720]),
    ("g=x", [0o0614, 0o0715, 0o7717, 0o0710]),
    ("g=X", [0o0604, 0o0715, 0o7717, 0o0710]),
    ("g=s", [0o2604, 0o2705, 0o7707, 0o2700]),
    ("g=t", [0o1604, 0o1705, 0o5707, 0o1700]),
    ("g=rw", [0o0664, 0o0765, 0o7767, 0o0760]),
    ("g=rwx", [0o0674, 0o0775, 0o7777, 0o0770]),
    ("g=rwxXst", [0o3664, 0o3775, 0o7777, 0o3770]),
    ("g=st", [0o3604, 0o3705, 0o7707, 0o3700]),
    ("g=sX", [0o2604, 0o2715, 0o7717, 0o2710]),
    ("g=ts", [0o3604, 0o3705, 0o7707, 0o3700]),
    ("o+r", [0o0644, 0o0755, 0o7777, 0o0754]),
    ("o+w", [0o0646, 0o0757, 0o7777, 0o0752]),
    ("o+x", [0o0645, 0o0755, 0o7777, 0o0751]),
    ("o+X", [0o0644, 0o0755, 0o7777, 0o0751]),
    ("o+s", [0o4644, 0o4755, 0o7777, 0o4750]),
    ("o+t", [0o1644, 0o1755, 0o7777, 0o1750]),
    ("o+rw", [0o0646, 0o0757, 0o7777, 0o0756]),
    ("o+rwx", [0o0647, 0o0757, 0o7777, 0o0757]),
    ("o+rwxXst", [0o5646, 0o5757, 0o7777, 0o5757]),
    ("o+st", [0o5644, 0o5755, 0o7777, 0o5750]),
    ("o+sX", [0o4644, 0o4755, 0o7777, 0o4751]),
    ("o+ts", [0o4644, 0o4755, 0o7777, 0o4750]),
    ("o-r", [0o0640, 0o0751, 0o7773, 0o0750]),
    ("o-w", [0o0644, 0o0755, 0o7775, 0o0750]),
    ("o-x", [0o0644, 0o0754, 0o7776, 0o0750]),
    ("o-X", [0o0644, 0o0754, 0o7776, 0o0750]),
    ("o-s", [0o0644, 0o0755, 0o3777, 0o0750]),
    ("o-t", [0o0644, 0o0755, 0o6777, 0o0750]),
    ("o-rw", [0o0640, 0o0751, 0o7771, 0o0750]),
    ("o-rwx", [0o0640, 0o0750, 0o7770, 0o0750]),
    ("o-rwxXst", [0o0640, 0o0750, 0o2770, 0o0750]),
    ("o-st", [0o0644, 0o0755, 0o2777, 0o0750]),
    ("o-sX", [0o0644, 0o0754, 0o3776, 0o0750]),
    ("o-ts", [0o0644, 0o0755, 0o3777, 0o0750]),
    ("o=r", [0o0644, 0o0754, 0o7774, 0o0754]),
    ("o=w", [0o0642, 0o0752, 0o7772, 0o0752]),
    ("o=x", [0o0641, 0o0751, 0o7771, 0o0751]),
    ("o=X", [0o0640, 0o0751, 0o7771, 0o0751]),
    ("o=s", [0o4640, 0o4750, 0o7770, 0o4750]),
    ("o=t", [0o1640, 0o1750, 0o7770, 0o1750]),
    ("o=rw", [0o0646, 0o0756, 0o7776, 0o0756]),
    ("o=rwx", [0o0647, 0o0757, 0o7777, 0o0757]),
    ("o=rwxXst", [0o5646, 0o5757, 0o7777, 0o5757]),
    ("o=st", [0o5640, 0o5750, 0o7770, 0o5750]),
    ("o=sX", [0o4640, 0o4751, 0o7771, 0o4751]),
    ("o=ts", [0o4640, 0o4750, 0o7770, 0o4750]),
    ("a+r", [0o0644, 0o0755, 0o7777, 0o0754]),
    ("a+w", [0o0666, 0o0777, 0o7777, 0o0772]),
    ("a+x", [0o0755, 0o0755, 0o7777, 0o0751]),
    ("a+X", [0o0644, 0o0755, 0o7777, 0o0751]),
    ("a+s", [0o6644, 0o6755, 0o7777, 0o6750]),
    ("a+t", [0o1644, 0o1755, 0o7777, 0o1750]),
    ("a+rw", [0o0666, 0o0777, 0o7777, 0o0776]),
    ("a+rwx", [0o0777, 0o0777, 0o7777, 0o0777]),
    ("a+rwxXst", [0o7666, 0o7777, 0o7777, 0o7777]),
    ("a+st", [0o7644, 0o7755, 0o7777, 0o7750]),
    ("a+sX", [0o6644, 0o6755, 0o7777, 0o6751]),
    ("a+ts", [0o7644, 0o7755, 0o7777, 0o7750]),
    ("a-r", [0o0200, 0o0311, 0o7333, 0o0310]),
    ("a-w", [0o0444, 0o0555, 0o7555, 0o0550]),
    ("a-x", [0o0644, 0o0644, 0o7666, 0o0640]),
    ("a-X", [0o0644, 0o0644, 0o7666, 0o0640]),
    ("a-s", [0o0644, 0o0755, 0o1777, 0o0750]),
    ("a-t", [0o0644, 0o0755, 0o6777, 0o0750]),
    ("a-rw", [0o0000, 0o0111, 0o7111, 0o0110]),
    ("a-rwx", [0o0000, 0o0000, 0o7000, 0o0000]),
    ("a-rwxXst", [0o0000, 0o0000, 0o0000, 0o0000]),
    ("a-st", [0o0644, 0o0755, 0o0777, 0o0750]),
    ("a-sX", [0o0644, 0o0644, 0o1666, 0o0640]),
    ("a-ts", [0o0644, 0o0755, 0o0777, 0o0750]),
    ("a=r", [0o0444, 0o0444, 0o7444, 0o0444]),
    ("a=w", [0o0222, 0o0222, 0o7222, 0o0222]),
    ("a=x", [0o0111, 0o0111, 0o7111, 0o0111]),
    ("a=X", [0o0000, 0o0111, 0o7111, 0o0111]),
    ("a=s", [0o6000, 0o6000, 0o7000, 0o6000]),
    ("a=t", [0o1000, 0o1000, 0o1000, 0o1000]),
    ("a=rw", [0o0666, 0o0666, 0o7666, 0o0666]),
    ("a=rwx", [0o0777, 0o0777, 0o7777, 0o0777]),
    ("a=rwxXst", [0o7666, 0o7777, 0o7777, 0o7777]),
    ("a=st", [0o7000, 0o7000, 0o7000, 0o7000]),
    ("a=sX", [0o6000, 0o6111, 0o7111, 0o6111]),
    ("a=ts", [0o7000, 0o7000, 0o7000, 0o7000]),
    ("ug+r", [0o0644, 0o0755, 0o7777, 0o0750]),
    ("ug+w", [0o0664, 0o0775, 0o7777, 0o0770]),
    ("ug+x", [0o0754, 0o0755, 0o7777, 0o0750]),
    ("ug+X", [0o0644, 0o0755, 0o7777, 0o0750]),
    ("ug+s", [0o6644, 0o6755, 0o7777, 0o6750]),
    ("ug+t", [0o1644, 0o1755, 0o7777, 0o1750]),
    ("ug+rw", [0o0664, 0o0775, 0o7777, 0o0770]),
    ("ug+rwx", [0o0774, 0o0775, 0o7777, 0o0770]),
    ("ug+rwxXst", [0o7664, 0o7775, 0o7777, 0o7770]),
    ("ug+st", [0o7644, 0o7755, 0o7777, 0o7750]),
    ("ug+sX", [0o6644, 0o6755, 0o7777, 0o6750]),
    ("ug+ts", [0o7644, 0o7755, 0o7777, 0o7750]),
    ("ug-r", [0o0204, 0o0315, 0o7337, 0o0310]),
    ("ug-w", [0o0444, 0o0555, 0o7557, 0o0550]),
    ("ug-x", [0o0644, 0o0645, 0o7667, 0o0640]),
    ("ug-X", [0o0644, 0o0645, 0o7667, 0o0640]),
    ("ug-s", [0o0644, 0o0755, 0o1777, 0o0750]),
    ("ug-t", [0o0644, 0o0755, 0o6777, 0o0750]),
    ("ug-rw", [0o0004, 0o0115, 0o7117, 0o0110]),
    ("ug-rwx", [0o0004, 0o0005, 0o7007, 0o0000]),
    ("ug-rwxXst", [0o0004, 0o0005, 0o0007, 0o0000]),
    ("ug-st", [0o0644, 0o0755, 0o0777, 0o0750]),
    ("ug-sX", [0o0644, 0o0645, 0o1667, 0o0640]),
    ("ug-ts", [0o0644, 0o0755, 0o0777, 0o0750]),
    ("ug=r", [0o0444, 0o0445, 0o7447, 0o0440]),
    ("ug=w", [0o0224, 0o0225, 0o7227, 0o0220]),
    ("ug=x", [0o0114, 0o0115, 0o7117, 0o0110]),
    ("ug=X", [0o0004, 0o0115, 0o7117, 0o0110]),
    ("ug=s", [0o6004, 0o6005, 0o7007, 0o6000]),
    ("ug=t", [0o1004, 0o1005, 0o1007, 0o1000]),
    ("ug=rw", [0o0664, 0o0665, 0o7667, 0o0660]),
    ("ug=rwx", [0o0774, 0o0775, 0o7777, 0o0770]),
    ("ug=rwxXst", [0o7664, 0o7775, 0o7777, 0o7770]),
    ("ug=st", [0o7004, 0o7005, 0o7007, 0o7000]),
    ("ug=sX", [0o6004, 0o6115, 0o7117, 0o6110]),
    ("ug=ts", [0o7004, 0o7005, 0o7007, 0o7000]),
    ("go+r", [0o0644, 0o0755, 0o7777, 0o0754]),
    ("go+w", [0o0666, 0o0777, 0o7777, 0o0772]),
    ("go+x", [0o0655, 0o0755, 0o7777, 0o0751]),
    ("go+X", [0o0644, 0o0755, 0o7777, 0o0751]),
    ("go+s", [0o2644, 0o2755, 0o7777, 0o2750]),
    ("go+t", [0o1644, 0o1755, 0o7777, 0o1750]),
    ("go+rw", [0o0666, 0o0777, 0o7777, 0o0776]),
    ("go+rwx", [0o0677, 0o0777, 0o7777, 0o0777]),
    ("go+rwxXst", [0o3666, 0o3777, 0o7777, 0o3777]),
    ("go+st", [0o3644, 0o3755, 0o7777, 0o3750]),
    ("go+sX", [0o2644, 0o2755, 0o7777, 0o2751]),
    ("go+ts", [0o3644, 0o3755, 0o7777, 0o3750]),
    ("go-r", [0o0600, 0o0711, 0o7733, 0o0710]),
    ("go-w", [0o0644, 0o0755, 0o7755, 0o0750]),
    ("go-x", [0o0644, 0o0744, 0o7766, 0o0740]),
    ("go-X", [0o0644, 0o0744, 0o7766, 0o0740]),
    ("go-s", [0o0644, 0o0755, 0o5777, 0o0750]),
    ("go-t", [0o0644, 0o0755, 0o6777, 0o0750]),
    ("go-rw", [0o0600, 0o0711, 0o7711, 0o0710]),
    ("go-rwx", [0o0600, 0o0700, 0o7700, 0o0700]),
    ("go-rwxXst", [0o0600, 0o0700, 0o4700, 0o0700]),
    ("go-st", [0o0644, 0o0755, 0o4777, 0o0750]),
    ("go-sX", [0o0644, 0o0744, 0o5766, 0o0740]),
    ("go-ts", [0o0644, 0o0755, 0o4777, 0o0750]),
    ("go=r", [0o0644, 0o0744, 0o7744, 0o0744]),
    ("go=w", [0o0622, 0o0722, 0o7722, 0o0722]),
    ("go=x", [0o0611, 0o0711, 0o7711, 0o0711]),
    ("go=X", [0o0600, 0o0711, 0o7711, 0o0711]),
    ("go=s", [0o2600, 0o2700, 0o7700, 0o2700]),
    ("go=t", [0o1600, 0o1700, 0o5700, 0o1700]),
    ("go=rw", [0o0666, 0o0766, 0o7766, 0o0766]),
    ("go=rwx", [0o0677, 0o0777, 0o7777, 0o0777]),
    ("go=rwxXst", [0o3666, 0o3777, 0o7777, 0o3777]),
    ("go=st", [0o3600, 0o3700, 0o7700, 0o3700]),
    ("go=sX", [0o2600, 0o2711, 0o7711, 0o2711]),
    ("go=ts", [0o3600, 0o3700, 0o7700, 0o3700]),
    ("ugo+r", [0o0644, 0o0755, 0o7777, 0o0754]),
    ("ugo+w", [0o0666, 0o0777, 0o7777, 0o0772]),
    ("ugo+x", [0o0755, 0o0755, 0o7777, 0o0751]),
    ("ugo+X", [0o0644, 0o0755, 0o7777, 0o0751]),
    ("ugo+s", [0o6644, 0o6755, 0o7777, 0o6750]),
    ("ugo+t", [0o1644, 0o1755, 0o7777, 0o1750]),
    ("ugo+rw", [0o0666, 0o0777, 0o7777, 0o0776]),
    ("ugo+rwx", [0o0777, 0o0777, 0o7777, 0o0777]),
    ("ugo+rwxXst", [0o7666, 0o7777, 0o7777, 0o7777]),
    ("ugo+st", [0o7644, 0o7755, 0o7777, 0o7750]),
    ("ugo+sX", [0o6644, 0o6755, 0o7777, 0o6751]),
    ("ugo+ts", [0o7644, 0o7755, 0o7777, 0o7750]),
    ("ugo-r", [0o0200, 0o0311, 0o7333, 0o0310]),
    ("ugo-w", [0o0444, 0o0555, 0o7555, 0o0550]),
    ("ugo-x", [0o0644, 0o0644, 0o7666, 0o0640]),
    ("ugo-X", [0o0644, 0o0644, 0o7666, 0o0640]),
    ("ugo-s", [0o0644, 0o0755, 0o1777, 0o0750]),
    ("ugo-t", [0o0644, 0o0755, 0o6777, 0o0750]),
    ("ugo-rw", [0o0000, 0o0111, 0o7111, 0o0110]),
    ("ugo-rwx", [0o0000, 0o0000, 0o7000, 0o0000]),
    ("ugo-rwxXst", [0o0000, 0o0000, 0o0000, 0o0000]),
    ("ugo-st", [0o0644, 0o0755, 0o0777, 0o0750]),
    ("ugo-sX", [0o0644, 0o0644, 0o1666, 0o0640]),
    ("ugo-ts", [0o0644, 0o0755, 0o0777, 0o0750]),
    ("ugo=r", [0o0444, 0o0444, 0o7444, 0o0444]),
    ("ugo=w", [0o0222, 0o0222, 0o7222, 0o0222]),
    ("ugo=x", [0o0111, 0o0111, 0o7111, 0o0111]),
    ("ugo=X", [0o0000, 0o0111, 0o7111, 0o0111]),
    ("ugo=s", [0o6000, 0o6000, 0o7000, 0o6000]),
    ("ugo=t", [0o1000, 0o1000, 0o1000, 0o1000]),
    ("ugo=rw", [0o0666, 0o0666, 0o7666, 0o0666]),
    ("ugo=rwx", [0o0777, 0o0777, 0o7777, 0o0777]),
    ("ugo=rwxXst", [0o7666, 0o7777, 0o7777, 0o7777]),
    ("ugo=st", [0o7000, 0o7000, 0o7000, 0o7000]),
    ("ugo=sX", [0o6000, 0o6111, 0o7111, 0o6111]),
    ("ugo=ts", [0o7000, 0o7000, 0o7000, 0o7000]),
];

/// Starting modes the permission-copy tables are evaluated against, in
/// [`COPY_MATRIX`] column order. Every probe carries three *distinct* owner /
/// group / other triads so a copy that lands in the wrong class is visible, and
/// `07124` additionally has all three special bits set to exercise the
/// destination-class clearing a `=` copy performs. The last probe is a
/// directory.
const COPY_PROBES: [(u32, bool); 4] = [
    (0o0761, false),
    (0o7124, false),
    (0o0405, false),
    (0o0752, true),
];

/// `(spec, [result per COPY_PROBES entry])` for the permission-copy grid, as
/// produced by upstream rsync 3.5.0.
const COPY_MATRIX: &[(&str, [u32; 4])] = &[
    ("+u", [0o0775, 0o7135, 0o0445, 0o0757]),
    ("+g", [0o0765, 0o7324, 0o0405, 0o0757]),
    ("+o", [0o0771, 0o7564, 0o0555, 0o0752]),
    ("-u", [0o0020, 0o7024, 0o0001, 0o0002]),
    ("-g", [0o0121, 0o7124, 0o0405, 0o0202]),
    ("-o", [0o0660, 0o7120, 0o0000, 0o0552]),
    ("=u", [0o0755, 0o0111, 0o0444, 0o0755]),
    ("=g", [0o0644, 0o0200, 0o0000, 0o0555]),
    ("=o", [0o0111, 0o0444, 0o0555, 0o0200]),
    ("u+u", [0o0761, 0o7124, 0o0405, 0o0752]),
    ("u+g", [0o0761, 0o7324, 0o0405, 0o0752]),
    ("u+o", [0o0761, 0o7524, 0o0505, 0o0752]),
    ("u-u", [0o0061, 0o7024, 0o0005, 0o0052]),
    ("u-g", [0o0161, 0o7124, 0o0405, 0o0252]),
    ("u-o", [0o0661, 0o7124, 0o0005, 0o0552]),
    ("u=u", [0o0761, 0o3124, 0o0405, 0o0752]),
    ("u=g", [0o0661, 0o3224, 0o0005, 0o0552]),
    ("u=o", [0o0161, 0o3424, 0o0505, 0o0252]),
    ("g+u", [0o0771, 0o7134, 0o0445, 0o0772]),
    ("g+g", [0o0761, 0o7124, 0o0405, 0o0752]),
    ("g+o", [0o0771, 0o7164, 0o0455, 0o0772]),
    ("g-u", [0o0701, 0o7124, 0o0405, 0o0702]),
    ("g-g", [0o0701, 0o7104, 0o0405, 0o0702]),
    ("g-o", [0o0761, 0o7124, 0o0405, 0o0752]),
    ("g=u", [0o0771, 0o5114, 0o0445, 0o0772]),
    ("g=g", [0o0761, 0o5124, 0o0405, 0o0752]),
    ("g=o", [0o0711, 0o5144, 0o0455, 0o0722]),
    ("o+u", [0o0767, 0o7125, 0o0405, 0o0757]),
    ("o+g", [0o0767, 0o7126, 0o0405, 0o0757]),
    ("o+o", [0o0761, 0o7124, 0o0405, 0o0752]),
    ("o-u", [0o0760, 0o7124, 0o0401, 0o0750]),
    ("o-g", [0o0761, 0o7124, 0o0405, 0o0752]),
    ("o-o", [0o0760, 0o7120, 0o0400, 0o0750]),
    ("o=u", [0o0767, 0o6121, 0o0404, 0o0757]),
    ("o=g", [0o0766, 0o6122, 0o0400, 0o0755]),
    ("o=o", [0o0761, 0o6124, 0o0405, 0o0752]),
    ("a+u", [0o0777, 0o7135, 0o0445, 0o0777]),
    ("a+g", [0o0767, 0o7326, 0o0405, 0o0757]),
    ("a+o", [0o0771, 0o7564, 0o0555, 0o0772]),
    ("a-u", [0o0000, 0o7024, 0o0001, 0o0000]),
    ("a-g", [0o0101, 0o7104, 0o0405, 0o0202]),
    ("a-o", [0o0660, 0o7120, 0o0000, 0o0550]),
    ("a=u", [0o0777, 0o0111, 0o0444, 0o0777]),
    ("a=g", [0o0666, 0o0222, 0o0000, 0o0555]),
    ("a=o", [0o0111, 0o0444, 0o0555, 0o0222]),
    ("ug+u", [0o0771, 0o7134, 0o0445, 0o0772]),
    ("ug+g", [0o0761, 0o7324, 0o0405, 0o0752]),
    ("ug+o", [0o0771, 0o7564, 0o0555, 0o0772]),
    ("ug-u", [0o0001, 0o7024, 0o0005, 0o0002]),
    ("ug-g", [0o0101, 0o7104, 0o0405, 0o0202]),
    ("ug-o", [0o0661, 0o7124, 0o0005, 0o0552]),
    ("ug=u", [0o0771, 0o1114, 0o0445, 0o0772]),
    ("ug=g", [0o0661, 0o1224, 0o0005, 0o0552]),
    ("ug=o", [0o0111, 0o1444, 0o0555, 0o0222]),
    ("go+u", [0o0777, 0o7135, 0o0445, 0o0777]),
    ("go+g", [0o0767, 0o7126, 0o0405, 0o0757]),
    ("go+o", [0o0771, 0o7164, 0o0455, 0o0772]),
    ("go-u", [0o0700, 0o7124, 0o0401, 0o0700]),
    ("go-g", [0o0701, 0o7104, 0o0405, 0o0702]),
    ("go-o", [0o0760, 0o7120, 0o0400, 0o0750]),
    ("go=u", [0o0777, 0o4111, 0o0444, 0o0777]),
    ("go=g", [0o0766, 0o4122, 0o0400, 0o0755]),
    ("go=o", [0o0711, 0o4144, 0o0455, 0o0722]),
    ("ugo+u", [0o0777, 0o7135, 0o0445, 0o0777]),
    ("ugo+g", [0o0767, 0o7326, 0o0405, 0o0757]),
    ("ugo+o", [0o0771, 0o7564, 0o0555, 0o0772]),
    ("ugo-u", [0o0000, 0o7024, 0o0001, 0o0000]),
    ("ugo-g", [0o0101, 0o7104, 0o0405, 0o0202]),
    ("ugo-o", [0o0660, 0o7120, 0o0000, 0o0550]),
    ("ugo=u", [0o0777, 0o0111, 0o0444, 0o0777]),
    ("ugo=g", [0o0666, 0o0222, 0o0000, 0o0555]),
    ("ugo=o", [0o0111, 0o0444, 0o0555, 0o0222]),
];

/// Multi-clause specs, as produced by upstream rsync 3.5.0. These pin the
/// resolution ORDER: a copy reads the mode left by the clauses before it, so
/// `u=g,g=o` and `g=o,u=g` must differ, and a copy composes with octal and
/// literal clauses and with the `D`/`F` selectors.
const COPY_SEQUENCE_MATRIX: &[(&str, [u32; 4])] = &[
    ("u=g,g=o", [0o0611, 0o1244, 0o0055, 0o0522]),
    ("g=o,u=g", [0o0111, 0o1444, 0o0555, 0o0222]),
    ("g=u,u=g", [0o0771, 0o1114, 0o0445, 0o0772]),
    ("u+g,g+o", [0o0771, 0o7364, 0o0455, 0o0772]),
    ("g-u,u-g", [0o0701, 0o7124, 0o0405, 0o0702]),
    ("700,u=g", [0o0000, 0o0000, 0o0000, 0o0000]),
    ("u=g,700", [0o0700, 0o0700, 0o0700, 0o0700]),
    ("u=g,u=o", [0o0161, 0o3424, 0o0505, 0o0252]),
    ("a=u,g-o", [0o0707, 0o0101, 0o0404, 0o0707]),
    ("Du=g,F=o", [0o0111, 0o0444, 0o0555, 0o0552]),
    ("u=g,g+x", [0o0671, 0o3234, 0o0015, 0o0552]),
    ("g+x,u=g", [0o0771, 0o3334, 0o0115, 0o0552]),
    ("o=u,u=o", [0o0767, 0o2121, 0o0404, 0o0757]),
];

/// Permission halves upstream rsync 3.5.0 REJECTS, out of every one- and
/// two-letter string over `rwxXstugoa`. The verdict is independent of the
/// who-class and the operator, so this set is keyed on the half alone.
const REJECTED_RHS: &[&str] = &[
    "Xa", "Xg", "Xo", "Xu", "a", "aX", "aa", "ag", "ao", "ar", "as", "at", "au", "aw", "ax", "gX",
    "ga", "gg", "go", "gr", "gs", "gt", "gu", "gw", "gx", "oX", "oa", "og", "oo", "or", "os", "ot",
    "ou", "ow", "ox", "ra", "rg", "ro", "ru", "sa", "sg", "so", "su", "ta", "tg", "to", "tu", "uX",
    "ua", "ug", "uo", "ur", "us", "ut", "uu", "uw", "ux", "wa", "wg", "wo", "wu", "xa", "xg", "xo",
    "xu",
];
/// Real `FileType` values for the file and directory probes; `tweak_mode()`
/// branches on `S_ISDIR` for both the `D`/`F` selector and the conditional `X`.
fn probe_file_types() -> (std::fs::FileType, std::fs::FileType) {
    let temp = tempfile::tempdir().expect("tempdir");
    let file_path = temp.path().join("f");
    let dir_path = temp.path().join("d");
    std::fs::write(&file_path, b"payload").expect("write file");
    std::fs::create_dir(&dir_path).expect("create dir");
    (
        std::fs::metadata(&file_path)
            .expect("file metadata")
            .file_type(),
        std::fs::metadata(&dir_path)
            .expect("dir metadata")
            .file_type(),
    )
}

/// Evaluates every `(spec, probe)` cell of `matrix` and asserts it reproduces
/// upstream's value.
fn assert_matrix(matrix: &[(&str, [u32; 4])], probes: &[(u32, bool); 4]) {
    let (file_type, dir_type) = probe_file_types();

    for (spec, expected) in matrix {
        let clauses = parse_with_umask(spec, UMASK).unwrap_or_else(|e| panic!("`{spec}`: {e}"));
        for (probe, want) in probes.iter().zip(expected) {
            let (mode, is_dir) = *probe;
            let file_type = if is_dir { dir_type } else { file_type };
            let got = apply_clauses(&clauses, mode, file_type) & 0o7777;
            assert_eq!(
                got,
                *want,
                "--chmod={spec} on {mode:04o} ({}) gave {got:04o}, upstream gives {want:04o}",
                if is_dir { "dir" } else { "file" }
            );
        }
    }
}

/// Expected row for `spec`, which must be present in `matrix`.
fn lookup(matrix: &[(&str, [u32; 4])], spec: &str) -> [u32; 4] {
    matrix
        .iter()
        .find(|(s, _)| *s == spec)
        .unwrap_or_else(|| panic!("matrix is missing `{spec}`"))
        .1
}

/// Every matrix cell must reproduce upstream's `tweak_mode()` output.
#[test]
fn chmod_matrix_matches_upstream() {
    assert_matrix(MATRIX, &PROBES);
}

/// The matrix must cover every `{who} x {op} x {perm}` combination it claims to,
/// so a row silently dropped from the table cannot shrink the guard.
#[test]
fn chmod_matrix_covers_the_full_grid() {
    for who in WHO {
        for op in OP {
            for what in WHAT {
                let spec = format!("{who}{op}{what}");
                assert!(
                    MATRIX.iter().any(|(s, _)| *s == spec),
                    "matrix is missing `{spec}`"
                );
            }
        }
    }
    assert_eq!(MATRIX.len(), WHO.len() * OP.len() * WHAT.len());
}

/// `a+s` sets BOTH set-id bits while `u+s` / `g+s` stay single-bit.
///
/// upstream: rsync 3.5.0 NEWS.md - "--chmod=a+s now sets both the setuid and
/// setgid bits, matching chmod(1) (it previously set setuid only)"; pinned by
/// upstream's own testsuite/chmod-setid_test.py.
#[test]
fn a_plus_s_sets_both_setid_bits() {
    let temp = tempfile::tempdir().expect("tempdir");
    let file_path = temp.path().join("f");
    std::fs::write(&file_path, b"payload").expect("write file");
    let file_type = std::fs::metadata(&file_path)
        .expect("file metadata")
        .file_type();

    let apply = |spec: &str| {
        let clauses = parse_with_umask(spec, UMASK).expect("parses");
        apply_clauses(&clauses, 0o644, file_type) & 0o7777
    };

    assert_eq!(apply("a+s"), 0o6644);
    assert_eq!(apply("u+s"), 0o4644);
    assert_eq!(apply("g+s"), 0o2644);
}

/// Every permission-copy cell must reproduce upstream's `tweak_mode()` output.
///
/// upstream: rsync 3.5.0 chmod.c `mode_copy_bits()` - `u=g` and friends read
/// the source class out of the mode and spread it across the destination
/// classes. rsync 3.4.4 rejected the whole form, so this is the grid that a
/// regression would silently take back to a parse error.
#[test]
fn chmod_copy_matrix_matches_upstream() {
    assert_matrix(COPY_MATRIX, &COPY_PROBES);
}

/// The copy matrix must cover every `{who} x {op} x {source-class}` triple it
/// claims to, so a row silently dropped from the table cannot shrink the guard.
#[test]
fn chmod_copy_matrix_covers_the_full_grid() {
    for who in WHO {
        for op in OP {
            for src in COPY_SOURCES {
                let spec = format!("{who}{op}{src}");
                assert!(
                    COPY_MATRIX.iter().any(|(s, _)| *s == spec),
                    "copy matrix is missing `{spec}`"
                );
            }
        }
    }
    assert_eq!(COPY_MATRIX.len(), WHO.len() * OP.len() * COPY_SOURCES.len());
}

/// A copy reads the mode as the *preceding* clauses left it, not the mode the
/// file started with, so a comma-separated list is order-dependent.
///
/// upstream: chmod.c `tweak_mode()` calls `mode_copy_bits(mode, ...)` inside the
/// clause loop, before that clause's own AND/OR is applied.
#[test]
fn chmod_copy_sequences_match_upstream() {
    assert_matrix(COPY_SEQUENCE_MATRIX, &COPY_PROBES);
}

/// Reordering two copy clauses must change the result - the pin that would fail
/// if the source were ever read from the original mode instead of the running
/// one.
#[test]
fn chmod_copy_is_order_dependent() {
    let forward = lookup(COPY_SEQUENCE_MATRIX, "u=g,g=o");
    let reverse = lookup(COPY_SEQUENCE_MATRIX, "g=o,u=g");
    assert_ne!(forward, reverse);
}

/// The grammar must not widen: for every `{who} x {op} x {one- or two-letter
/// permission half}` spec, oc's accept/reject decision must equal upstream
/// 3.5.0's.
///
/// upstream: chmod.c parse_chmod() STATE_2ND_HALF - a copy letter may not be
/// combined with literal bits, with `s`/`t`, or with a second copy letter, in
/// either order, and `a` is never a copy source. The verdict depends only on
/// the permission half, which is why [`REJECTED_RHS`] is keyed on it.
#[test]
fn chmod_permission_half_verdicts_match_upstream() {
    const LETTERS: [char; 10] = ['r', 'w', 'x', 'X', 's', 't', 'u', 'g', 'o', 'a'];

    let mut halves = Vec::with_capacity(LETTERS.len() * (LETTERS.len() + 1));
    for a in LETTERS {
        halves.push(a.to_string());
        for b in LETTERS {
            halves.push(format!("{a}{b}"));
        }
    }
    assert_eq!(
        REJECTED_RHS.len(),
        halves
            .iter()
            .filter(|h| REJECTED_RHS.contains(&h.as_str()))
            .count(),
        "REJECTED_RHS holds an entry outside the enumerated universe"
    );

    for who in WHO {
        for op in OP {
            for half in &halves {
                let spec = format!("{who}{op}{half}");
                let rejected = parse_with_umask(&spec, UMASK).is_err();
                assert_eq!(
                    rejected,
                    REJECTED_RHS.contains(&half.as_str()),
                    "`{spec}`: oc {} it, upstream 3.5.0 does not",
                    if rejected { "rejects" } else { "accepts" }
                );
            }
        }
    }
}

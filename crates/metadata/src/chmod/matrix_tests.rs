//! Table-driven `--chmod` conformance matrix pinned to upstream rsync.
//!
//! The defect class this guards is "a who-letter does not expand to every bit
//! it covers": `--chmod=a+s` set setuid only, because the `a` clause left the
//! set-id top bits unset and the `s` clause fell through to its no-who default.
//! A single `a+s` assertion would not catch the siblings of that bug (`a-s`,
//! `a=s`, `a+st`, `a=t`, ...), so the whole `{who} x {op} x {perm}` grid is
//! evaluated here.
//!
//! upstream: chmod.c `parse_chmod()` + `tweak_mode()`. Every expected value in
//! [`MATRIX`] was produced by compiling rsync 3.5.0's `chmod.c` verbatim
//! against a stubbed `rsync.h` and printing `tweak_mode()` for each spec and
//! probe, so the table is upstream's own output rather than a hand-derivation.

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

/// Every matrix cell must reproduce upstream's `tweak_mode()` output.
#[test]
fn chmod_matrix_matches_upstream() {
    let temp = tempfile::tempdir().expect("tempdir");
    let file_path = temp.path().join("f");
    let dir_path = temp.path().join("d");
    std::fs::write(&file_path, b"payload").expect("write file");
    std::fs::create_dir(&dir_path).expect("create dir");
    let file_type = std::fs::metadata(&file_path)
        .expect("file metadata")
        .file_type();
    let dir_type = std::fs::metadata(&dir_path)
        .expect("dir metadata")
        .file_type();

    for (spec, expected) in MATRIX {
        let clauses = parse_with_umask(spec, UMASK).unwrap_or_else(|e| panic!("`{spec}`: {e}"));
        for (probe, want) in PROBES.iter().zip(expected) {
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

/// The matrix must cover every `{who} x {op} x {perm}` combination it claims to,
/// so a row silently dropped from the table cannot shrink the guard.
#[test]
fn chmod_matrix_covers_the_full_grid() {
    const WHO: [&str; 8] = ["", "u", "g", "o", "a", "ug", "go", "ugo"];
    const OP: [&str; 3] = ["+", "-", "="];
    const WHAT: [&str; 12] = [
        "r", "w", "x", "X", "s", "t", "rw", "rwx", "rwxXst", "st", "sX", "ts",
    ];

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

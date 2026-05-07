# BLAKE2b-256 strong checksum: negotiation feasibility for protocol 32

Tracking issue: oc-rsync task #1836.
Related upstream reference: `target/interop/upstream-src/rsync-3.4.1/checksum.c`,
`target/interop/upstream-src/rsync-3.4.1/compat.c`.
Project policy reference: `feedback_no_wire_protocol_features.md` -
oc-rsync MUST stay byte-equivalent to upstream rsync 3.4.1 on every
non-batch wire path.

This is a design judgement, not an implementation plan. No code lands
in this PR. The output is a go/no-go decision and, if "go", the
boundary that decision must respect.

## Summary

BLAKE2b-256 is an attractive strong checksum on technical merit:
cryptographic strength on par with SHA-256, throughput around three
cycles per byte on modern x86_64 (faster than SHA-256 by 2x-3x without
SHA-NI hardware), no instruction-set requirements, and a 32-byte
digest that fits the existing strong-checksum slot used for SHA-256.
The temptation is to add it as a fourth negotiable algorithm beside
MD5, XXH3-64 and XXH3-128.

The conclusion of this note is that BLAKE2b-256 cannot be added to the
oc-rsync wire negotiation. Upstream rsync 3.4.1 does not list BLAKE2
in `valid_checksums_items[]` or `valid_auth_checksums_items[]` and
does not parse it from `--checksum-choice`. Advertising a name the
upstream peer does not know would either be silently dropped (best
case) or cause the negotiation handshake to surface an "unknown
checksum name" error and exit with `RERR_UNSUPPORTED` (worst case).
Either outcome violates the wire-compatibility invariant.

Two narrow non-wire uses remain viable and are described in section 6
as future work: BLAKE2b-256 as the engine for `--checksum=blake2b` of
local-only basis verification, and as an internal content-addressed
key for in-process dedupe. Neither touches the bytes that flow on the
network.

## 1. Current strong checksum algorithms

`crates/checksums/src/strong/strategy.rs` (and its decomposed
submodules under `crates/checksums/src/strong/strategy/`) defines the
runtime-selectable strong checksum set:

| Kind         | Digest len | Used for                                        |
|--------------|-----------:|-------------------------------------------------|
| `Md4`        | 16         | Protocol < 30, file-list checksum at proto 27.  |
| `Md5`        | 16         | Default for protocol >= 30, transfer + file.    |
| `Sha1`       | 20         | Daemon authentication only.                     |
| `Sha256`     | 32         | Daemon authentication only.                     |
| `Sha512`     | 64         | Daemon authentication only.                     |
| `Xxh64`      | 8          | Negotiated via `--checksum-choice=xxhash`.      |
| `Xxh3`       | 8          | Negotiated via `--checksum-choice=xxh3`.        |
| `Xxh3_128`   | 16         | Negotiated via `--checksum-choice=xxh128`.      |

The selector layer (`strategy/selector.rs`) maps protocol version to a
default kind, and `--checksum-choice=NAME` overrides via
`ChecksumAlgorithmKind::from_name`. The transfer-time checksum used in
the delta protocol is bounded by `MAX_DIGEST_LEN = 64` so that raw
bytes can be copied into `sum_struct.sum2` without further allocation.

Negotiation happens during the legacy server-args exchange. The
capability string `-e.LsfxCIvu` is sent by the client; the trailing
`v` advertises the willingness to do the post-handshake checksum
negotiation block, and `u` enables XXH variants. See
`build_capability_string()` in `crates/core/src/client/setup.rs`.

## 2. Why BLAKE2b-256 looks attractive

BLAKE2b is the 64-bit-tuned member of the BLAKE2 family. Outputting
256 bits gives 128-bit collision resistance, identical to SHA-256.
The relevant data points:

- Throughput. Software BLAKE2b benchmarks at roughly 3 cycles per byte
  on x86_64 (single core, 8 KiB buffer) - about 2x faster than
  software SHA-256 and about 4x faster than SHA-512 on the same
  hardware. On AArch64 without SHA-2 instructions the gap is larger.
- No hardware requirement. BLAKE2b is built from ARX (add / rotate /
  xor) primitives and runs at near-peak speed on any 64-bit CPU.
  Unlike SHA-256, performance does not collapse on cores that lack
  SHA-NI / Crypto extensions.
- 32-byte digest. The output fits the existing SHA-256 slot:
  `MAX_DIGEST_LEN` already supports 64-byte digests, the
  block-checksum header reserves room for 16-byte sums by default,
  and `sum_struct.sum2` length is chosen at session start. No struct
  rework is needed to carry a 32-byte digest.
- Cryptographic strength. As of 2026 BLAKE2b is unbroken in the
  collision and preimage senses. It is a credible upgrade path away
  from MD5 for any role that today uses MD5 because "everyone has
  it", not because MD5 is appropriate.
- Maintained. The Rust ecosystem has a vetted constant-time
  implementation in `blake2` (RustCrypto), already a transitive
  dependency through `password-hash` users. No new C library is
  introduced.

If wire compatibility were not a constraint, BLAKE2b-256 would be the
right default strong checksum.

## 3. Wire negotiation: what it would take, and why it cannot ship

Adding a fourth negotiable algorithm would require all of:

1. A new `ChecksumAlgorithmKind::Blake2b256` variant with
   `digest_len() == 32` and a `from_name("blake2b" | "blake2b-256")`
   match arm.
2. An `impls::Blake2b256Strategy` that wraps `blake2::Blake2b<U32>`,
   honours the `SeedConfig` rules (seed mixed before length-prefixed
   data, matching MD5 ordering at protocol >= 30), and zero-extends
   the digest into `ChecksumDigest` with no allocation in the hot
   path.
3. Extension of `--checksum-choice` parsing in
   `crates/cli/src/frontend/...` so the flag accepts `blake2b`.
4. Extension of the over-the-wire negotiation. This is where the
   proposal fails. Upstream rsync 3.4.1 negotiates checksum
   algorithms via the `valid_checksums` `name_num_obj` exchanged in
   `compat.c:setup_protocol()`. The ordered candidate list is built
   from `valid_checksums_items[]` in `checksum.c`:

   ```c
   struct name_num_item valid_checksums_items[] = {
   #ifdef SUPPORT_XXH3
       { CSUM_XXH3_128, 0, "xxh128", NULL },
       { CSUM_XXH3_64,  0, "xxh3",   NULL },
   #endif
   #ifdef SUPPORT_XXHASH
       { CSUM_XXH64,    0, "xxh64",  NULL },
       { CSUM_XXH64,    0, "xxhash", NULL },
   #endif
       { CSUM_MD5, NNI_BUILTIN|NNI_EVP, "md5", NULL },
       { CSUM_MD4, NNI_BUILTIN|NNI_EVP, "md4", NULL },
   #ifdef SHA_DIGEST_LENGTH
       { CSUM_SHA1, NNI_EVP, "sha1", NULL },
   #endif
       { CSUM_NONE, 0, "none", NULL },
       { 0, 0, NULL, NULL }
   };
   ```

   There is no `blake2b` entry. `parse_csum_name()` calls
   `get_nni_by_name()`, and on miss it logs `unknown checksum name`
   and `exit_cleanup(RERR_UNSUPPORTED)`. Sending the literal token
   `blake2b` on the wire to an unmodified upstream peer is therefore
   not a soft fallback - it is a hard transfer abort with exit code
   3.

A name that is not in the upstream candidate list also breaks the
ordered intersection algorithm in `recv_negotiate_str()`: upstream
filters the received list by what it recognises before picking the
highest mutually known entry. An oc-rsync-only name would be dropped
during that filter pass on the upstream side, and the negotiated
result would diverge between peers when both happen to be oc-rsync
(advertising and selecting `blake2b`) versus mixed (where upstream
silently drops the unknown name and falls back to MD5). That
divergence is exactly the silent incompatibility the project policy
forbids.

Even gating advertisement behind a peer-id sniff does not help. The
negotiation block is exchanged before any application traffic, and
oc-rsync identifies as upstream rsync 3.4.1 over the wire by design.
There is no peer fingerprint available at negotiation time that
distinguishes "another oc-rsync" from "real upstream" without
introducing an out-of-band signal - which would itself be a wire
extension.

## 4. Compatibility regime if it were enabled

The design that would be safe in isolation - "advertise BLAKE2b-256
only when the peer also advertises it; fall back to MD5 otherwise" -
is precisely the design upstream's negotiator already implements for
known names. The blocker is not the fallback; the blocker is that the
candidate list itself is part of the wire contract. Adding a name
upstream does not list expands that list unilaterally.

The fallback proposed in the task statement (MD5 when BLAKE2b is not
mutually agreed) is correct in shape. It is also moot: if BLAKE2b is
not in the advertised list, the negotiator never sees it and falls
back to MD5 by default. Implementing the fallback adds zero new
behaviour because MD5 is already the protocol-30 default.

## 5. Upstream check: is BLAKE2 already negotiable?

Searched `target/interop/upstream-src/rsync-3.4.1/` for `blake`,
`BLAKE`, `b2sum`, `blake2`. Zero matches. BLAKE2 is not a build
option, not a configure flag, not a member of any `name_num_item`
table, and not referenced in `checksum.c`, `compat.c`, `options.c`,
`flist.c`, or `match.c`. It is not negotiable upstream and there is
no in-flight upstream patch series for BLAKE2 as of 3.4.1.

This makes any oc-rsync negotiation entry a non-upstream extension by
construction. Per `feedback_no_wire_protocol_features.md`, non-upstream
wire extensions are rejected.

## 6. Recommendation

The wire-protocol path is closed. The decision is **no-go on wire
negotiation**. Two non-wire uses remain on the table and are
inexpensive to deliver later if there is demand. They are listed for
completeness; neither is in scope for task #1836.

### 6a. Local-only `--checksum=blake2b` for basis verification

`--checksum` (the `-c` flag) computes a strong digest of every
candidate file before transfer and compares it locally. The digest
never leaves the host on which it was computed. The wire still
carries MD5 (or whichever algorithm was negotiated) for the actual
delta-protocol checksum. A `--checksum-algorithm=blake2b` extension
that gates only the `-c` pre-flight comparison can be added without
touching `valid_checksums_items` or any wire byte. This would deliver
the speed and cryptographic-strength benefits for the common
"verify-then-skip" use case at the cost of one new strategy impl and
a CLI alias.

This remains an oc-rsync-only flag. It does not survive a
`--write-batch` replay on upstream, and it must be documented as a
local optimisation that has no over-the-wire effect. If accepted, it
should be guarded by an explicit "no wire impact" test: a fixture
that asserts the byte stream is identical with and without the flag
on the same source tree.

### 6b. Internal content-addressed dedupe key

Several engine paths (buffer cache lookups, hardlink coalescing,
crash-safe partial-file resume) hash file content for in-process
keying. Today these use MD5 because MD5 is already wired in.
Switching the in-process hash to BLAKE2b-256 is a pure local change
with no wire visibility. It is an opportunistic refactor, not a
feature.

### 6c. What does NOT happen

- No new entry in any `name_num_item`-equivalent table that crosses
  the wire.
- No new `--checksum-choice=blake2b` value parsed against the
  negotiated set.
- No new capability bit added to the `-e.LsfxCIvu` string.
- No advertisement of BLAKE2b in the `@RSYNCD:` daemon greeting or
  the post-handshake checksum negotiation block.

## 7. Decision

Close task #1836 as no-go for wire negotiation. The project policy
that wire extensions diverging from upstream rsync 3.4.1 are
forbidden takes precedence over the technical merit of the algorithm.
If demand for local cryptographic verification appears, file a
follow-up scoped to section 6a (`--checksum=blake2b` local-only) with
a wire-byte parity test included from the start.

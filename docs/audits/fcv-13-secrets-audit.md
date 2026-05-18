# FCV-13 - Secrets file fuzz coverage audit

Tracking issue: #2439. Companion to `docs/audits/fcv-3-fuzz-coverage-gaps.md`
and `docs/audits/fuzz-coverage-matrix.md`.

## 1. Scope

FCV-13 asked whether `SecretsFile::parse` warranted a dedicated fuzz target,
or whether the combined `auth_response` target landed in PR #4444 already
provides adequate panic-freedom coverage of both pre-auth parsers exposed by
the daemon:

- `daemon::auth::verify_client_response` - consumes the base64 response a
  client sends in reply to `@RSYNCD: AUTHREQD`. Length-disambiguates across
  MD4, MD5, SHA-1, SHA-256, and SHA-512.
- `daemon::auth::SecretsFile::parse` - reads `username:password` entries
  from the admin secrets file. Malformed lines must surface as `io::Error`,
  never panic the daemon at startup.

Both functions are reachable before any credential validation, so a panic
in either is a daemon-availability bug.

## 2. Selector distribution in `auth_response.rs`

The combined target at `fuzz/fuzz_targets/auth_response.rs` peels the first
input byte and dispatches on `selector & 0b11`:

| `selector & 0b11` | Frequency | `verify_client_response` exercised | `SecretsFile::parse` exercised |
|---|---|---|---|
| `0` | 25% | yes | no |
| `1` | 25% | no | yes |
| `2` | 25% | yes | yes |
| `3` | 25% | yes | yes |

Aggregate per-input reach:

- `verify_client_response`: **75%** of non-empty inputs.
- `SecretsFile::parse`: **75%** of non-empty inputs.

Both surfaces receive identical coverage budget. Neither dominates the
fuzzing time, so the libFuzzer coverage feedback rewards mutations against
either parser independently. The two-bit mask was chosen specifically so
the high-arity branch exercises both surfaces, preventing the trivial bias
that would arise from a strict 50/50 split.

## 3. Input shape per branch

### `fuzz_verify_response`

- Splits the remaining payload three ways (password, challenge, response).
- Rejects non-UTF-8 challenge or response, so the fuzzer learns to keep
  those slices ASCII via the coverage signal.
- Cycles `proto_byte % 8` through `None`, `28..=32`, `0`, and `255` to
  exercise the unknown-protocol branch alongside every supported negotiated
  version.

### `fuzz_secrets_parse`

- Accepts any UTF-8 payload and feeds it directly to `SecretsFile::parse`.
- Reaches the multi-line loop, the `\r` trim, comment / empty-line skip,
  the `split_once(':')` happy path, and the `InvalidData` error path on
  lines that lack a colon separator.
- The seed corpus entry (`fuzz/corpus/auth_response/seed_basic`,
  `alice dGVzdHJlc3BvbnNlcGFkZGluZw`) primes the verifier path; once the
  selector flips, libFuzzer rapidly synthesises colon-bearing lines because
  inserting `:` strictly grows the coverage bitmap on the secrets branch.

## 4. Decision

**Leave the combined `auth_response` target as-is. No code change for
FCV-13.**

Rationale:

1. Both functions receive a 75% reach budget per input - neither is
   starved.
2. The two surfaces share a corpus, which is the desired property here:
   bytes that look like a base64 response on one selector value look like a
   malformed secrets line on another, and the fuzzer reuses one mutation
   pool for both. Splitting would force the corpus to be re-grown twice.
3. `SecretsFile::parse` has a tiny state space (line loop, comment skip,
   single `split_once`). The libFuzzer coverage frontier is exhausted in
   seconds; a dedicated target would add maintenance burden without
   measurable coverage gain.
4. The target follows the precedent set by `protocol_wire`,
   `multiplex_frame_parse`, and `filter_differential`, all of which
   dispatch on a leading selector byte to keep related parsers behind a
   single binary.

## 5. Re-evaluation trigger

Re-open FCV-13 only if any of the following hold:

- A regression adds new branching to `SecretsFile::parse` (e.g. quoted
  passwords, multi-character separators, line continuations) that the
  combined corpus cannot reach within a 10-minute fuzzing window.
- A regression adds expensive setup to `verify_client_response` (e.g.
  per-call HMAC key derivation) that makes the shared budget unfair to the
  secrets branch.
- A panic or hang is discovered on one branch and the corpus split would
  meaningfully accelerate triage.

Until then the single binary at `fuzz/fuzz_targets/auth_response.rs`
satisfies FCV-13's coverage requirement.

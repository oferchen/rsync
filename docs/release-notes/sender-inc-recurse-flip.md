# Sender-side INC_RECURSE on by default (push transfers)

Scaffold for the release blurb that ships with the ISI.h flip-default PR
(#2745). Numbers and PR refs are placeholders until that PR lands.

## What changed

Sender-side INC_RECURSE is now enabled by default for push transfers
(client-as-sender). The default was disabled in v0.6.2 to mitigate a
push-path regression introduced in v0.6.1 - see
`docs/audit/v061-daemon-push-regression.md` (V61D-1) for the root cause.
The ISI series (#2737-#2746) re-validated the sender-INC_RECURSE path
against upstream rsync 3.0.9 / 3.1.3 / 3.4.1 / 3.4.2 and closed the
performance gap that motivated the v0.6.2 revert.

## User impact

Significantly faster sender-side start-time on large source trees: the
sender no longer walks the full file list before transmitting the first
segment. ISI.g bench (PR #4862) measured the win on a 100K-file source:
**start-time: `<filled in at ISI.h flip>` -> `<filled in at ISI.h flip>`
(<filled in at ISI.h flip>x faster)**. Wire bytes and final transfer
totals are unchanged.

## What to test

- Daemon-push and SSH-push against upstream rsync 3.4.x on your real
  workloads.
- Watch for any latency, wall-clock, or wire-byte regression vs the prior
  release.
- Confirm `--itemize-changes` output is byte-identical.

## How to revert

- **Runtime per invocation:** pass `--no-inc-recursive` on the client.
  There is no environment variable; the CLI flag is the runtime override.
  (The `sender-inc-recurse` cargo feature that gated this during the ISI
  bake-in period was retired in ISI.i.2; there is no build-time toggle.)

## Reference

- V61D-1 audit: `docs/audit/v061-daemon-push-regression.md`
- ISI series tracking issues: #2737-#2746 (ISI.h flip: #2745)
- ISI.g bench (start-time win on 100K-file source): PR #4862
- Flip PR: `<filled in at ISI.h flip>`

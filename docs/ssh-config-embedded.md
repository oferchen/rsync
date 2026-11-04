# ssh_config in the embedded `ssh://` transport

With the `embedded-ssh` feature, an `ssh://` operand reads `~/.ssh/config` and `/etc/ssh/ssh_config` itself instead of handing them to an `ssh` binary. The reader follows OpenSSH 10.0p1 (`readconf.c`, `ssh.c`), the pinned reference release. Every citation in the reader names that tree.

## How parity is checked

`crates/rsync_io/src/ssh/embedded/ssh_config_differential.rs` resolves each configuration with oc's reader and with `ssh -G` from a 10.0p1 build. It then compares the dumped rows.

- CI builds that release and points the harness at it with `OC_RSYNC_SSH_G_BIN`.
- `OC_RSYNC_REQUIRE_SSH_G=1` turns a missing oracle into a failure instead of a skip.

The epic gate is `corpus_matches_the_oracle_on_every_resolved_row`. Its corpus covers:

- bastions with `ProxyJump` and `Match`;
- `Include` directories with globs, and nested includes;
- `%h`/`%%` expansion;
- the `Match` criteria oc evaluates (`host`, `originalhost`, `all`, `final`, `canonical`, `tagged`);
- the algorithm lists, `RekeyLimit` and `Tag`;
- canonicalization without DNS.

The gate fails on any row where both sides resolve a value and disagree, and lists every divergence. A floor on the number of matched rows keeps it from passing vacuously.

The one tolerated difference is representation, not value. `ssh -G` dumps `IdentityFile` before expanding `~`, while oc dumps it after.

Refusals have their own cells, which compare oc's error text with `ssh -G`'s stderr byte for byte.

## Behaviour changes

These differ from oc-rsync releases before 0.6.5. Each now matches OpenSSH 10.0p1.

### Errors and refusals

- An unknown keyword refuses the configuration. Every bad line is reported in file order, followed by `<file>: terminating, N bad configuration options` (`readconf.c:2596-2613`). Earlier releases skipped unknown keywords silently and stopped at the first refused line.
- `IgnoreUnknown` excuses only the unknown keywords that come after it (`readconf.c:977-979`).
- A keyword with no value is refused with `no argument after keyword "<kw>"` (`readconf.c:1190-1193`).
- An invalid `Ciphers`, `MACs`, `KexAlgorithms` or `HostKeyAlgorithms` list, or a bad `RekeyLimit`, is refused, even inside a `Host` block that does not match.
- The five `Canonicalize*` keywords are validated, with upstream's refusal texts.

### Keywords that now take effect

- Every keyword OpenSSH accepts still loads, including the ones the transport does not act on. `RemoteCommand` is accepted but never replaces `rsync --server`.
- `IdentityFile2` is read as `IdentityFile`.
- `Ciphers`, `MACs`, `KexAlgorithms`, `HostKeyAlgorithms` and `RekeyLimit` change what the transport offers and when it re-keys. The `+`, `-` and `^` forms work.
- `Tag` and `Match tagged` work.

### Host names and the final pass

- `CanonicalizeHostname yes|always` canonicalizes the host by trying each `CanonicalDomains` suffix, then re-reads the configuration, so `Match canonical` blocks apply.
- `Match !final` requests the final pass, as `Match final` does.
- When a final pass runs, the host is lowercased (addresses excepted). A `HostName` in a final-pass block no longer replaces it (`ssh.c:1286-1290`).

## Design decisions

- **Composition base.** `+`, `-` and `^` compose against the transport's own tuned defaults: the AES-NI-aware cipher order, the fast key-exchange order, and russh's MAC and host-key lists. They do not compose against OpenSSH's defaults.
- **Unsupported names.** An algorithm OpenSSH knows but the transport cannot negotiate is dropped with one warning naming it. Examples are `umac-*`, `hmac-md5*`, `sntrup761*`, security-key and certificate host-key types. A list with nothing negotiable left fails the connection.

## Known deviations

These are tracked for a follow-up (task 594):

- **RekeyLimit:**
  - sizes are capped at russh's 1 GiB maximum, and `default` keeps that 1 GiB, where OpenSSH derives the limit from the cipher;
  - with no interval, russh still re-keys hourly;
  - an interval of `0` turns time-based re-keying off.
- **HostKeyAlgorithms order:** the list is not reordered by the keys already in `known_hosts`.
- **Late composition errors:** a list that validates but cannot be composed (a stray `!` pattern) fails at connect time rather than when the file is read.
- **Unvalidated values:** the values of keywords the transport does not act on are not validated (task 589).
- **No CNAME following:** `CanonicalizePermittedCNAMEs` is parsed but CNAMEs are not followed, and the bare-host resolve check that goes with it is skipped (task 593).
- **No address reformatting:** numeric addresses are not reformatted the way `resolve_addr` reformats them.
- **Error prefix:** `Could not resolve host "x"` is printed without OpenSSH's `ssh: ` prefix.
- **10.6-only keywords:** `RefuseConnection` and `WarnWeakCrypto` exist only in OpenSSH 10.6 and are refused as unknown, as 10.0p1 refuses them.

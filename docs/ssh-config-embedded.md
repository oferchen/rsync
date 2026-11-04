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
- The value of every keyword the transport does not act on is checked by its `readconf.c` arm, with upstream's refusal text. For example, `ForwardAgent maybe/x${` is refused, as are `LocalForward 0 host:80`, `ControlMaster maybe` and `TCPKeepAlive transport`.
- A line with more values than its keyword takes is refused with `keyword <kw> extra arguments at end of line` (`readconf.c:2529-2534`).
- The keywords the transport acts on get the same checks:
  - an empty value (`User ""`) is refused with `Missing argument.`;
  - an absent flag value prints `missing argument.` and then `unsupported option "(null)".`;
  - `Port` must resolve through `a2port` (`Bad port 'abc'.`), and a service name such as `Port ssh` resolves;
  - `IdentitiesOnly` takes only a flag, and `IdentityAgent` gets the `${VAR}` and `$NAME` checks;
  - a 101st `IdentityFile` or `CertificateFile` is refused.
- Time values follow 10.0p1's `convtime`: whole seconds only, so `ConnectTimeout 1.5` is refused and `5s5` means 10 seconds.

### Keywords that now take effect

- Every keyword OpenSSH accepts still loads, including the ones the transport does not act on. `RemoteCommand` is accepted but never replaces `rsync --server`.
- `IdentityFile2` is read as `IdentityFile`.
- `Ciphers`, `MACs`, `KexAlgorithms`, `HostKeyAlgorithms` and `RekeyLimit` change what the transport offers and when it re-keys. The `+`, `-` and `^` forms work.
- `Tag` and `Match tagged` work.
- A numeric host is rewritten the way `resolve_addr` rewrites it, whether or not canonicalization is on: `127.1` becomes `127.0.0.1` and `0:0::1` becomes `::1`. That form is what the transport connects to and looks up in `known_hosts`. A scoped IPv6 address keeps its scope as written.
- With no `HostKeyAlgorithms`, or a `+`/`-` list, the host-key types `known_hosts` already records for the host are offered first, unless the first preference is already recorded (`sshconnect2.c:119-206`). A server with several host keys is then verified against the recorded one, where earlier releases negotiated ed25519 and reported the host as unknown.
- A `Ciphers`, `MACs` or `KexAlgorithms` list that passes validation but cannot be composed, such as `^aes128-ctr,,!aes256-ctr`, no longer refuses the connection. As in OpenSSH, it prints `fill_default_options: <list>: invalid argument` and falls back to the defaults (`readconf.c:2794`, `:2987-2997`). The lists after it in the order ciphers, MACs, key exchange are then used exactly as written, without `+`/`-`/`^` composition or pattern expansion; a list written as `+name` therefore names nothing negotiable and is refused. `HostKeyAlgorithms` still fails at connect, now as `ssh_kex2: kex_assemble_namelist: invalid argument` (`sshconnect2.c:245-247`).
- `CanonicalizePermittedCNAMEs` is honoured. A canonicalized name, or a name canonicalization left alone, takes the CNAME its lookup reports when a rule's source list matches the name and its target list matches the CNAME (`ssh.c:387-421`). With rules set, a host canonicalization did not resolve is looked up as typed, and a direct connection to a name that does not resolve is refused as `ssh: Could not resolve hostname <host>: <reason>`, even with `CanonicalizeHostname no` (`ssh.c:1255-1273`).
- Time-based re-keying happens only when `RekeyLimit` names an interval above `0`, as in OpenSSH. Before, the transport re-keyed every hour regardless.

### Host names and the final pass

- `CanonicalizeHostname yes|always` canonicalizes the host by trying each `CanonicalDomains` suffix, then re-reads the configuration, so `Match canonical` blocks apply.
- `Match !final` requests the final pass, as `Match final` does.
- When a final pass runs, the host is lowercased (addresses excepted). A `HostName` in a final-pass block no longer replaces it (`ssh.c:1286-1290`).

## Design decisions

- **Composition base.** `+`, `-` and `^` compose against the transport's own tuned defaults: the AES-NI-aware cipher order, the fast key-exchange order, and russh's MAC and host-key lists. They do not compose against OpenSSH's defaults.
- **Unsupported names.** An algorithm OpenSSH knows but the transport cannot negotiate is dropped with one warning naming it. Examples are `umac-*`, `hmac-md5*`, `sntrup761*`, security-key and certificate host-key types. A list with nothing negotiable left fails the connection.
- **GSSAPI and PKCS#11.** Which keywords exist depends on how OpenSSH was built (`readconf.c:212-226`). oc models a build with both, as Linux distributions ship: `GSSAPIAuthentication` and `GSSAPIDelegateCredentials` take a yes/no flag, and `PKCS11Provider` and `SmartcardDevice` take a string. Bad values are refused with upstream's text, and good values load silently. The pinned 10.0p1 oracle is built without GSSAPI and prints `Unsupported option` for those keywords instead. Mirroring it would print that error for every stock Debian or Fedora configuration, so the GSSAPI arms are pinned by unit tests rather than oracle cells.
- **Service names.** A port given as a service name (`Port ssh`, `LocalForward 8080 host:http`) is looked up in the services file, which is `getservbyname`'s `files` source. A name served only by another NSS source is not found.

## Known deviations

These are tracked for a follow-up (task 594):

- **RekeyLimit size (accepted):** sizes above 1 GiB are capped at russh's 1 GiB maximum, and `default` keeps that 1 GiB. OpenSSH derives a far higher limit from the cipher (`packet.c:1033-1043`). russh refuses anything larger, so the only cost is an extra re-key per GiB transferred.
- **Fill-time notes:** OpenSSH prints some notes after the file loads, such as `fill_default_options: ca_sign_algorithms: invalid argument` and `Cannot forward agent socket path`. The transport never uses those values, so oc does not print the notes.
- **Skipped none-clearing after a failed list:** when a list fails to compose, OpenSSH also skips the rest of `fill_default_options` (`readconf.c:3003-3041`), so `RevokedHostKeys none` and similar `none` values are not cleared and upstream then tries to load a file called `none`. oc still clears them. Only that combination differs, because `ProxyCommand none` and `ProxyJump none` are re-checked where they are used (`ssh.c:466`, `:1263`).
- **10.6-only keywords:** `RefuseConnection` and `WarnWeakCrypto` exist only in OpenSSH 10.6 and are refused as unknown, as 10.0p1 refuses them.

# UTS-22.b status: GHSA-rjfm-3w2m-jf4f closure verification

Status: **CLOSED** by commit `50f389f50` (PR #5524, merged 2026-06-10).
Audit date: 2026-06-11.

## 1. Advisory reference

- GHSA ID: GHSA-rjfm-3w2m-jf4f
- CVE alias: CVE-2026-43617
- Summary: Reverse-DNS lookup after daemon chroot causes hostname ACL bypass.
- Internal repository advisory; tracked in `SECURITY.md` known-CVE matrix
  (line 72).

## 2. Vulnerability sketch

Hostname-based `hosts deny` rules silently fail OPEN when the daemon runs
under `daemon chroot` and the chroot lacks NSS configuration
(`/etc/resolv.conf`, `/etc/nsswitch.conf`, `/etc/hosts`, NSS shared
objects). Per-connection reverse DNS executes inside the chroot, returns
nothing, and the matcher cannot prove the peer's name matches the deny
pattern, so the connection is admitted. An attacker who controls their
PTR record - or simply blackholes reverse DNS - bypasses any hostname-
pattern deny rule. Upstream rsync 3.4.3 (commit c38f20c5) closes the bug
by moving the reverse DNS lookup to before chroot/setuid in the per-
connection `start_daemon()`. oc-rsync uses a thread-per-connection model
where `daemon chroot` is applied process-wide at startup before any peer
connects, so the upstream "resolve before the chroot in this connection"
sequence is not available - by the time a worker thread sees a new peer,
the process is already chrooted.

## 3. Fix location

- Commit: `50f389f50e96da5d34ea170f1ff714d85b5d185d`
- PR: #5524 (`fix: address GHSA-rjfm-3w2m-jf4f hostname ACL bypass during chroot`)
- Source: `crates/daemon/src/daemon/module_state/definition.rs` lines 156-220
  (`ModuleDefinition::permits`).
- Mechanism: fail-closed guard - when `hostname.is_none()` AND any
  `hosts deny` rule returns `requires_hostname()`, `permits()` returns
  `false`. IP-only deny rules and pure allow rules are unaffected.

## 4. Regression test coverage

Three regression tests ship alongside the fix under
`crates/daemon/src/tests/chunks/`:

| Test file                                                       | Scope |
|-----------------------------------------------------------------|-------|
| `module_hostname_deny_fails_closed_when_dns_unresolved.rs`      | GHSA scenario A - hostname-deny rule under unresolved reverse DNS. |
| `module_ip_deny_unaffected_by_dns_failure.rs`                   | Scope guard - IP-only deny keeps original semantics. |
| `module_peer_hostname_resolution_before_chroot_denies_unknown.rs` | Allow-side fail-closed when hostname resolution fails. |

Two adjacent peer-hostname tests
(`module_peer_hostname_missing_resolution_denies_hostname_only_rules.rs`,
`module_peer_hostname_skips_lookup_when_disabled.rs`,
`module_peer_hostname_uses_override.rs`) cover the surrounding allow-side
behaviour. Test module is registered in `crates/daemon/src/tests.rs`
(per commit stat: 2 lines added to the test index).

No follow-up `UTS-22.b.test` task required - coverage is complete.

## 5. SECURITY.md status check

`SECURITY.md:72` lists the advisory as **Fixed** with the full
explanation chain:

- Hostname resolution runs before chroot at session level
  (`session_runtime.rs::handle_session`) and module level
  (`module_access::request.rs::respond_with_module_request`,
  `listing.rs::respond_with_module_list`).
- Per-module chroot applied later in
  `transfer.rs::apply_privilege_restrictions_with_upstream_errors`.
- Global `daemon chroot` directive applied at startup in
  `accept_loop.rs::serve_connections`, before the accept loop.
- Fail-closed guard in `ModuleDefinition::permits` covers the post-chroot
  DNS failure path that the upstream pre-chroot strategy cannot reach for
  a thread-per-connection model.
- Three regression tests are cited inline.

No SECURITY.md update gap remains.

## 6. Verdict

**UTS-22.b is closed by PR #5524.** All exit criteria met:

- Fix shipped on master.
- Three regression tests cover the GHSA scenario, scope guard, and
  allow-side analogue.
- SECURITY.md correctly reclassifies the advisory from the prior "Not
  vulnerable" assessment to **Fixed** with the full multi-layer
  explanation.
- No follow-up sub-tasks needed under UTS-22.b.

Parent task **UTS-22 (#3589) stays pending** - sibling subtask UTS-22.a
(capture root-mode runtests.py log) is still pending and UTS-22.c is
deferred. Closing UTS-22.b alone does not close the parent.

## References

- Upstream `clientserver.c::allow_access()` (`match_hostname()` /
  `match_address()`).
- Upstream 3.4.3 commit c38f20c5 (move reverse DNS before chroot).
- `crates/daemon/src/daemon/module_state/definition.rs:156-220`.
- `SECURITY.md:72`.
- `docs/audits/security-rsync-3-4-3-cve-audit-2026-05-20.md` (3.4.3 CVE
  sweep that initially classified this advisory).

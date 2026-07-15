## Summary

Adds the two remaining upstream per-module `rsyncd.conf` logging parameters as
P_LOCAL directives, completing the per-module runtime-parameter work started in
#6610:

- **`syslog tag`** — the syslog ident (default `rsyncd` upstream; oc-rsync keeps
  its branded `oc-rsyncd` default so entries do not collide with upstream on a
  host running both).
- **`syslog facility`** — the syslog facility name (default `daemon`), accepting
  the upstream set `kern, user, mail, daemon, auth, syslog, lpr, news, uucp,
  cron, local0`..`local7`.

Previously oc honoured these only in the global section; a value inside a
`[module]` was warned as an unknown per-module directive and dropped, so
operators could not route a module's daemon logs to a distinct syslog tag or
facility.

## Behaviour

Both parameters are P_LOCAL, matching upstream `loadparm.c`:

- A `[module]` value overrides the global default for that module only.
- A module without the directive inherits the global-section value (or the
  built-in default), mirroring upstream's `init_section` copy semantics — so a
  module overriding only the tag still inherits the global facility, and vice
  versa.
- An unrecognised facility name is not a config error: the module silently keeps
  its inherited/default facility, matching upstream `loadparm.c` `case P_ENUM`
  which leaves the value unchanged when no enum entry matches.
- Empty values are rejected, consistent with oc's other module string
  directives.

When a module is selected for a connection and the daemon is in syslog mode
(no log file configured), the process-wide syslog handle is reopened with the
module's resolved tag/facility for the duration of that session and restored to
the daemon-global logger afterwards. This mirrors upstream `log.c` `log_init`,
which reopens syslog per selected module. oc serves connections on threads
sharing one syslog handle rather than in forked children, so an RAII guard
performs the restore that upstream gets for free from process exit. On Windows
(no syslog) the reconfiguration is a compile-time no-op.

## Upstream references

- `loadparm.c` — `syslog_tag` (P_STRING, P_LOCAL, default `rsyncd`),
  `syslog_facility` (P_ENUM, P_LOCAL, default `LOG_DAEMON`),
  `enum_syslog_facility[]`, and `case P_ENUM` keep-default-on-unknown.
- `log.c:169` `log_init` / `log.c:143` `openlog(lp_syslog_tag(module_id),
  LOG_PID, lp_syslog_facility(module_id))` — per-module syslog reopen.

## Tests

- Per-module override sets that module's tag/facility and never leaks into the
  global default; a sibling without the directive inherits the global value.
- Unknown facility name inherits silently (no parse error); empty value and
  duplicate directive are rejected.
- Cross-platform facility-name validator stays in sync with the Unix
  name-to-constant map.
- The scoped syslog reconfiguration guard restores the previously-active logger
  on drop.

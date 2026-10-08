# test-support

Shared test utilities for the oc-rsync workspace - provides helpers that multiple
test suites need, avoiding duplicated setup boilerplate across crates.

## Key Public Functions

- `create_tempdir`, `create_named_tempfile`, `create_canonical_tempdir` - create
  temporary paths named with a `.tmp<pid>-` prefix, so concurrent test processes
  never draw the same name

## Dependencies

- **Upstream:** `tempfile`
- **Downstream:** used as a `dev-dependency` by crates needing reliable temp directories

## Platform Notes

- Windows: a plain `tempfile` name can collide across concurrent nextest
  processes, and creating over another process's directory or delete-pending
  file fails with `PermissionDenied`, which `tempfile` does not retry. The
  per-process prefix makes such collisions impossible.

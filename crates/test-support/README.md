# test-support

Shared test utilities for the oc-rsync workspace - provides helpers that multiple
test suites need, avoiding duplicated setup boilerplate across crates.

## Key Public Functions

- `create_tempdir` - creates a temporary directory with retry logic for transient
  OS errors (Windows CI antivirus/lock contention)

## Dependencies

- **Upstream:** `tempfile`
- **Downstream:** used as a `dev-dependency` by crates needing reliable temp directories

## Platform Notes

- Windows: retries `tempdir()` up to 3 times with exponential backoff (50ms, 100ms,
  150ms) to handle antivirus and filesystem lock contention on CI runners
- Unix: typically succeeds on first attempt

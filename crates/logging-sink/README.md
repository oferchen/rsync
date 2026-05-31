# logging-sink

Message sink and output utilities - streams diagnostics to arbitrary writers
while reusing scratch buffers, matching upstream rsync's `log.c` approach.

## Key Public Types

- `MessageSink` - wrapper around `std::io::Write` with reusable scratch buffer
- `LineMode` - controls whether rendered messages end with a newline

## Modules

- `sink` - core `MessageSink` implementation
- `line_mode` - `LineMode` enum and rendering helpers
- `syslog` - Unix syslog backend for daemon-mode diagnostics

## Dependencies

- **Upstream:** `core` (provides `Message` and `MessageScratch` types)
- **Downstream:** `daemon`, `cli`

## Platform Notes

- Unix: provides a syslog backend via the `syslog` crate, routing daemon diagnostics
  through `syslog(3)` with configurable facility and tag
- Windows: syslog module compiles out; diagnostics go to stderr or Windows Event Log
  via the `platform` crate

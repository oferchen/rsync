# Upstream 3.4.2 parity: `--dirs` long option

Tracking issue: #2216. Verified 2026-05-14 against `origin/master`.

## 1. Upstream change

The rsync 3.4.2 NEWS file records the following parity fix:

> Added the missing `--dirs` long option.

Prior to 3.4.2, upstream rsync accepted only the short form `-d`; the long
form `--dirs` was documented but not wired into `long_options[]` in
`options.c`. The 3.4.2 release closes that gap so both spellings are
accepted.

## 2. oc-rsync status: already at parity

oc-rsync exposes both `-d` and `--dirs` from a single clap arg
definition. No code change is required.

Clap entry:

- File: `crates/cli/src/frontend/command_builder/sections/build_base_command/transfer.rs`
- Lines 50-57:

```rust
Arg::new("dirs")
    .long("dirs")
    .short('d')
    .help("Copy directory entries even when recursion is disabled.")
    .action(ArgAction::SetTrue)
    .overrides_with("no-dirs"),
```

The negated mirror `--no-dirs` (with visible alias `--no-d`) immediately
follows at lines 58-65. Both flags resolve into the same tri-state value
via `tri_state_flag_negative_first(&matches, "dirs", "no-dirs")` in
`crates/cli/src/frontend/arguments/parser/mod.rs:145`, which writes to
`ParsedArgs::dirs: Option<bool>`.

## 3. Supported-options list

`SUPPORTED_OPTIONS_LIST` in `crates/cli/src/frontend/defaults.rs:10`
already advertises both spellings:

```
--dirs/-d, --no-dirs, ...
```

The help / unknown-option diagnostic surfaces this string verbatim, so
users see `--dirs/-d` as a recognised flag.

## 4. Test coverage

Existing coverage in
`crates/cli/src/frontend/tests/parse_args_recognises_recursive.rs`:

- `parse_args_recognises_dirs_flag` - exercises `--dirs`.
- `parse_args_recognises_no_dirs_flag` - exercises `--no-dirs`.
- `parse_args_prefers_last_dirs_toggle` - last-wins between
  `--dirs` and `--no-dirs`.
- `parse_args_dirs_short_and_long_are_equivalent` (new) - confirms
  `-d` and `--dirs` produce the same `parsed.dirs` value, locking the
  3.4.2 parity claim into the test suite.

## 5. Conclusion

No production code change required. oc-rsync has accepted both `-d` and
`--dirs` since the clap definition was introduced. The newly added
unit test guards against future regression.

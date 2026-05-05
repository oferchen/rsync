# Audit: `--iconv` is parsed but inert on the local-copy and filter-rule paths

Tracking issue: oc-rsync task #1918. Branch: `docs/iconv-inert-audit-1918`.

## Summary

Upstream rsync 3.4.1 treats `--iconv=LOCAL,REMOTE` as a per-session
charset-conversion contract: outgoing filenames, symlink targets,
`--files-from` records, secluded args, and `MSG_*` text payloads are all
re-encoded from the local charset to a UTF-8 wire form before transmit,
and incoming bytes are re-encoded from UTF-8 back to the local charset
on receive. oc-rsync today parses `--iconv` cleanly, validates the
charset spec, stores it on `ClientConfig`, forwards it to the remote
peer in the SSH/daemon argv, and (since PR #3458) bridges it to a
concrete `FilenameConverter` for the SSH and daemon code paths via
`apply_common_server_flags`. What it does **not** do is apply that
converter on (a) the local-copy code path, (b) the filter-rule chain,
(c) the daemon module's per-module `charset` directive, or (d) the
symlink-target / files-from / secluded-args byte streams. The flag is
therefore inert end-to-end for any user who combines `--iconv` with a
local copy or who relies on filter-rule path matching across charsets.
The path to making it functional is a small set of producer-side
wirings to existing consumers (file-list reader, file-list writer,
filter chain, daemon `ServerConfig`); none of the wirings require a
wire-protocol extension or a new trait. The remediation is decomposed
across trackers #1911-#1917 and #1919, ordered so each follow-up is
independently mergeable.

This audit is the formal report for tracker #1918. It supersedes the
preliminary skeleton at `docs/audits/iconv-inert.md` (PR #3455) by
folding in the findings from PRs #3458, #3514, #3517, and #3424, and
by mapping every gap to a concrete oc-rsync source line that exists in
the tree today.

## Severity

**Critical for users who pass `--iconv` and rely on local-side
transcoding.** With `--iconv=utf-8,latin1` (or any non-identity charset
pair), the receiver / generator running on the local machine never
holds a `FilenameConverter` for the local-copy path, never threads one
into the filter chain, and never applies one to symlink targets even on
SSH or daemon transfers. Filenames containing non-ASCII bytes traverse
the engine as raw remote bytes; user-supplied include/exclude patterns
silently fail to match transcoded names; symlink targets with non-ASCII
content land mojibake on disk. The user receives no error, no warning,
and a successful exit code. The defect is not a stub or a `todo!()`.
Every type exists, every consumer is wired to call into the iconv hook
*if* a converter is present, and every constructor is implemented. The
defect is the missing producer call on the four hot paths listed above.

## Scope

In scope:

- Upstream rsync 3.4.1 reference behaviour for `--iconv` and the
  per-call-site `iconvbufs(ic_send/ic_recv, ...)` invocations.
- The current oc-rsync surface that parses, stores, and partially
  consumes `IconvSetting`.
- The four producer-side gaps (sender flist emit on local-copy,
  receiver flist ingest on local-copy, filter-rule path matching,
  daemon module `charset` directive).
- The dependency ordering between trackers #1911-#1919.
- Test surface (#1916, #1919) needed to validate the wiring.
- Risks and decisions: feature-gate behaviour, protocol-version
  compatibility, error reporting on conversion failure, charset
  normalization rules.

Out of scope:

- The `FilenameConverter` API design (covered by PR #3517 in
  `docs/audits/iconv-filename-converter-design.md`).
- The library-choice decision between `encoding_rs` and system iconv
  (covered by `docs/audits/iconv-feature-design.md`).
- The full `iconvbufs` call-site coverage table (covered by
  `docs/audits/iconv-pipeline.md`).
- The `IconvSetting` parse path itself (covered by PR #3514 in
  `docs/audits/iconv-parse-deadend.md`).

## Source files inspected

All file:line citations below are repository-relative. Inspected
crates: `cli`, `core`, `transfer`, `protocol`, `daemon`, `filters`,
plus workspace `Cargo.toml` and the per-crate `Cargo.toml` files
that gate the `iconv` feature. Upstream tree at
`target/interop/upstream-src/rsync-3.4.1/` was read for `rsync.c`,
`flist.c`, `compat.c`, `clientserver.c`, `options.c`, `io.c`. There
is no `iconv.c` in upstream; the `iconvbufs` helper lives in
`rsync.c`.

## 1. Upstream behavior reference

`--iconv=LOCAL,REMOTE` is documented in upstream `rsync.1` and
implemented by a small set of well-defined call sites. The salient
shape is:

- The wire bytes are always UTF-8. The user-visible `LOCAL,REMOTE`
  spec describes how local filesystem bytes (`LOCAL`) map to UTF-8
  wire bytes (`REMOTE`) and vice versa. Upstream's two-`iconv_t`
  split (`ic_send` for `local -> UTF-8`, `ic_recv` for
  `UTF-8 -> local`) reflects the C iconv API and is collapsed into a
  single `FilenameConverter` in oc-rsync.
- `--iconv=.` resolves both sides to the locale charset
  (`nl_langinfo(CODESET)` in upstream).
- `--iconv=-` and `--no-iconv` disable the conversion.
- Daemons may default a per-module charset via the `charset` keyword
  in the daemon configuration; the user can still override with
  `--no-iconv`.

### 1.1 Setup, parse, and global state

Key upstream sites for parsing and converter setup:

- `options.c:219, 814, 1366, 1396, 1416, 1668, 2052-2054, 2716-2722`
  - `iconv_opt` global, popt entry, parse, `--no-iconv` reset, the
  local/remote split (`strchr(iconv_opt, ',')`).
- `rsync.c:87-147` - `setup_iconv()` opens
  `ic_send = iconv_open(UTF8_CHARSET, charset)` (local -> wire) and
  `ic_recv = iconv_open(charset, UTF8_CHARSET)` (wire -> local).
  Both fail with `RERR_UNSUPPORTED` (exit 4) on `iconv_open`
  failure.
- `rsync.c:179` - `iconvbufs()` is the single helper invoked from
  every call site below.

### 1.2 Per-call-site application

Per-call-site `iconvbufs` invocations in the 3.4.1 tree
(file:line - direction - role):

- `flist.c:745` - `recv_file_entry()` filename, wire -> local.
- `flist.c:1141` - `recv_file_entry()` symlink target, wire ->
  local, gated on `sender_symlink_iconv`.
- `flist.c:1588, 1595` - `send_file_entry()` dirname/basename,
  local -> wire.
- `flist.c:1609` - `send_file_entry()` symlink target, local ->
  wire, gated on `symlink_len && sender_symlink_iconv`.
- `io.c:428, 445` - `forward_filesfrom_data()` per-record,
  local -> wire.
- `io.c:1025` - `send_msg()` `MSG_*` payload, local -> wire.
- `io.c:1282` - `read_line(..., RL_CONVERT)`, wire -> local.
- `io.c:1575` - `MSG_DELETED` path, local -> wire.
- `rsync.c:304` - `send_protected_args()` per arg, local -> wire,
  with `ICB_INCLUDE_BAD` (verbatim fallback).

### 1.3 Capability negotiation

- `target/interop/upstream-src/rsync-3.4.1/compat.c:716-718` -
  `compat_flags |= CF_SYMLINK_ICONV` is set in the local
  compatibility flags **only when** `ICONV_OPTION` is compiled in.
  In the 3.4.1 tarball this is unconditional within the
  `#ifdef ICONV_OPTION` block, but the bit only matters when the
  peer also has `iconv_opt` configured.
- `target/interop/upstream-src/rsync-3.4.1/compat.c:763-767` -
  `sender_symlink_iconv` is true only when **both** peers
  advertised `CF_SYMLINK_ICONV` and the local `iconv_opt` is
  non-null.

### 1.4 Daemon module config

- `target/interop/upstream-src/rsync-3.4.1/clientserver.c:712-717` -
  the daemon copies `iconv_opt = lp_charset(i)` for the requested
  module, calls `setup_iconv()`, then resets `iconv_opt = NULL`.
  The per-module `charset` directive is the upstream equivalent of
  oc-rsync's `crates/daemon/src/daemon/module_state/definition.rs`
  `charset: Option<String>` field.
- `target/interop/upstream-src/rsync-3.4.1/options.c:995-1011` -
  daemon-side popt processing of `iconv` against the module's
  refuse list.

### 1.5 Failure semantics summary

The upstream sites differ in how they treat lossy or invalid input:

| Site | Direction | On lossy / invalid bytes |
|---|---|---|
| `flist.c:745` recv filename | wire -> local | warn + drop entry |
| `flist.c:1141` recv symlink | wire -> local | warn + drop target |
| `flist.c:1588, 1595` send filename | local -> wire | hard error, exit |
| `flist.c:1609` send symlink | local -> wire | hard error, exit |
| `rsync.c:304` protected args | local -> wire | include verbatim (`ICB_INCLUDE_BAD`) |
| `io.c:1025` `MSG_*` body | local -> wire | include verbatim |

The receiver-side asymmetry (warn rather than abort) is why oc-rsync's
`FilenameConverter::remote_to_local` and `local_to_remote` both
return `Result`, leaving the policy decision at the call site.

## 2. Current oc-rsync implementation surface

The CLI-side parse path is healthy and lands `IconvSetting` on
`ClientConfig`. The transfer-side consumer wiring is fully present.
Two of the four hot paths now have a producer wired (SSH and daemon).
The other two do not.

### 2.1 Parse path (parses, stores, forwards)

- `crates/cli/src/frontend/command_builder/sections/connection_and_logging_options.rs:142-152` -
  clap `Arg::new("iconv").long("iconv").value_name("CONVERT_SPEC")`,
  `num_args(1)`, `OsStringValueParser`, `conflicts_with("no-iconv")`.
- `crates/cli/src/frontend/command_builder/sections/connection_and_logging_options.rs:154-159` -
  clap `Arg::new("no-iconv")`, `ArgAction::SetTrue`,
  `conflicts_with("iconv")`.
- `crates/cli/src/frontend/arguments/parser/mod.rs:318-319` -
  clap match extraction:
  `let iconv = matches.remove_one::<OsString>("iconv");
   let no_iconv = matches.get_flag("no-iconv");`.
- `crates/cli/src/frontend/execution/options/iconv.rs:29` -
  `pub(crate) fn resolve_iconv_setting(spec, disable) -> Result<IconvSetting, Message>`.
- `crates/cli/src/frontend/execution/options/iconv.rs:60-74` -
  `accept_parsed_setting`: on `not(feature = "iconv")` an explicit
  spec is rejected with `rsync_error!(1, ...)` (closes #1915).
- `crates/core/src/client/config/iconv.rs:25` -
  `IconvSetting::parse(spec)` validates the `LOCAL,REMOTE` form and
  recognises `.` (`LocaleDefault`) and `-` (`Disabled`).
- `crates/cli/src/frontend/execution/drive/workflow/run.rs:267` -
  `let iconv_setting = match resolve_iconv_setting(iconv.as_deref(), no_iconv) { ... };`.
- `crates/cli/src/frontend/execution/drive/workflow/run.rs:753` -
  `iconv: iconv_setting,` lands the value on `ConfigInputs`.
- `crates/cli/src/frontend/execution/drive/config.rs:132` -
  `pub(crate) iconv: IconvSetting`.
- `crates/cli/src/frontend/execution/drive/config.rs:253` -
  `.iconv(inputs.iconv.clone())` invokes the `ClientConfigBuilder`
  setter. **This is the only production call site of the setter**
  outside the `core` test suite.
- `crates/core/src/client/config/builder/network.rs:93` -
  `ClientConfigBuilder::iconv(setting: IconvSetting) -> Self`.
- `crates/core/src/client/config/client/mod.rs:169` -
  `pub(super) iconv: IconvSetting` on `ClientConfig`.
- `crates/core/src/client/config/client/mod.rs:323` - default is
  `iconv: IconvSetting::Unspecified`.
- `crates/core/src/client/config/client/mod.rs:355` -
  `pub const fn iconv(&self) -> &IconvSetting` accessor.

The full parse-path trace is in `docs/audits/iconv-parse-deadend.md`.

### 2.2 Wire-side re-emit (out-of-process consumer)

- `crates/core/src/client/config/iconv.rs:78` -
  `IconvSetting::cli_value(&self) -> Option<String>` re-renders the
  setting back into `--iconv=...` for the *remote* peer's argv. The
  remote peer parses it and applies its own iconv. The local
  process's role on this hop is purely string forwarding.

### 2.3 In-process consumer wiring (SSH and daemon)

PR #3458 closed the SSH / daemon bridge. The site is:

- `crates/core/src/client/remote/flags.rs:203-228` -
  `apply_common_server_flags(config, server_config)` reads
  `config.iconv()` and assigns
  `server_config.connection.iconv = config.iconv().resolve_converter();`.
- `crates/core/src/client/config/iconv.rs:130-151` -
  `IconvSetting::resolve_converter(&self) -> Option<FilenameConverter>`
  performs the translation:

  ```rust
  pub fn resolve_converter(&self) -> Option<FilenameConverter> {
      match self {
          Self::Unspecified | Self::Disabled => None,
          Self::LocaleDefault => Some(converter_from_locale()),
          Self::Explicit { local, remote } => {
              let remote_charset = remote.as_deref().unwrap_or(".");
              match FilenameConverter::new(local, remote_charset) {
                  Ok(converter) => Some(converter),
                  Err(_error) => {
                      #[cfg(feature = "tracing")]
                      tracing::warn!(/* ... */);
                      None
                  }
              }
          }
      }
  }
  ```

- The `apply_common_server_flags` call is invoked by every SSH and
  daemon entry point: SSH receiver and sender, embedded SSH receiver
  and sender, daemon receiver and sender. Verified by the parse-path
  audit (`docs/audits/iconv-parse-deadend.md` "Consumer 2").

### 2.4 Transfer-side hooks

The transfer crate accepts and consumes a converter:

- `crates/transfer/src/config/mod.rs:100-101` -
  `pub iconv: Option<FilenameConverter>` field on `ConnectionConfig`.
- `crates/transfer/src/config/builder.rs:183-187` -
  `ServerConfigBuilder::iconv(&mut self, converter: Option<FilenameConverter>) -> &mut Self`.
  The setter is exercised on the SSH / daemon path via
  `apply_common_server_flags`.
- `crates/transfer/src/receiver/mod.rs:369-370` -
  `if let Some(ref converter) = self.config.connection.iconv {
       reader = reader.with_iconv(converter.clone()); }`.
  Applied to the file-list reader on every receiver run.
- `crates/transfer/src/generator/mod.rs:564-565` - mirror of the
  above for the file-list writer (sender side, generator role).

### 2.5 File-list wire encode / decode hooks

Both wire-encoding sites have a fully implemented iconv hook that
activates when a converter is set:

- `crates/protocol/src/flist/read/name.rs:101-118` -
  `apply_encoding_conversion(&self, name: Vec<u8>) -> io::Result<Vec<u8>>`.
  Calls `converter.remote_to_local(&name)` when `self.iconv.is_some()`.
- `crates/protocol/src/flist/write/encoding.rs:308-327` -
  `apply_encoding_conversion(&self, name: &[u8]) -> io::Result<Cow<[u8]>>`.
  Calls `converter.local_to_remote(name)` when `self.iconv.is_some()`.
- `crates/protocol/src/flist/read/mod.rs:97` -
  `iconv: Option<FilenameConverter>` on `FileListReader`.
- `crates/protocol/src/flist/read/mod.rs:339` -
  `pub const fn with_iconv(mut self, converter: FilenameConverter) -> Self`.
- `crates/protocol/src/flist/read/mod.rs:628` -
  `let converted_name = self.apply_encoding_conversion(name)?;`
  invocation site inside the per-entry read loop.
- `crates/protocol/src/flist/write/mod.rs:94, 304, 377` - matching
  field, setter, and per-entry application on the writer side.

### 2.6 Conversion engine

- `crates/protocol/src/iconv/mod.rs:33` - re-exports
  `FilenameConverter`, `EncodingConverter`, `EncodingPair`,
  `converter_from_locale`.
- `crates/protocol/src/iconv/converter.rs:21` - the `FilenameConverter`
  struct definition (carries `&'static encoding_rs::Encoding` for
  `local_encoding` and `remote_encoding` when the feature is on; an
  empty struct when off).
- `crates/protocol/src/iconv/converter.rs:46-50` - `Default` returns
  the identity converter.
- `crates/protocol/src/iconv/converter.rs:52-389` - constructors
  (`identity`, `new`, `new_lenient`) and conversion helpers
  (`local_to_remote`, `remote_to_local`, `to_local`, `to_remote`).
- `crates/protocol/src/iconv/converter.rs:417-429` -
  `converter_from_locale()` free function.
- `crates/protocol/src/iconv/error.rs` - `ConversionError`,
  `EncodingError`.

### 2.7 Daemon-side surface

- `crates/daemon/src/daemon/sections/module_parsing.rs:21-37` -
  `VITAL_OPTIONS` includes `"iconv"` and `"no-iconv"`. These are
  *unrefusable*; the daemon will not let a refuse list strip
  `--iconv` from a client invocation.
- `crates/daemon/src/daemon/module_state/definition.rs:108-109` -
  `pub(crate) charset: Option<String>` on the module's resolved
  state. Populated from the per-module `charset` directive.
- `crates/daemon/src/daemon/module_state/definition.rs:413-415` -
  `pub(crate) fn charset(&self) -> Option<&str>` accessor.
- `crates/daemon/src/daemon/sections/config_parsing/module_directives.rs:330-337` -
  the `"charset" => { ... builder.set_charset(cs, path, line_number)?; }`
  directive parser, invoked while ingesting `oc-rsyncd.conf`.
- `crates/daemon/src/daemon/sections/module_definition/setters.rs:660-677` -
  `set_charset(...)` validates uniqueness within a module section.
- `crates/daemon/src/daemon/sections/config_parsing/tests.rs:695-703` -
  `parse_module_charset` test confirms the directive parses.

The directive is parsed and stored per-module. Repository-wide grep
for `definition.charset()`, `module.charset()`, `.charset()` returns
**only the test reference**
(`crates/daemon/src/daemon/sections/config_parsing/tests.rs:695`).
There is no production consumer that reads the resolved
`charset` and threads it into `ServerConfig.connection.iconv`.

### 2.8 Filter side

- `crates/filters/src/chain.rs:226` -
  `pub fn new(global: FilterSet) -> Self` constructor for
  `FilterChain`. The signature accepts only filter rules; there is
  no `Option<FilenameConverter>` parameter.
- `crates/transfer/src/generator/filters.rs:53, 80` and
  `crates/transfer/src/receiver/transfer.rs:895` - the three
  production call sites that build a `FilterChain`. None pass a
  converter.
- Repository-wide grep across `crates/filters/` for
  `iconv|FilenameConverter|EncodingConverter` returns zero matches.

### 2.9 Cargo gating

- `crates/protocol/Cargo.toml:27` - `iconv = ["encoding_rs"]`.
- `crates/protocol/Cargo.toml:38` - `encoding_rs = { version = "0.8", optional = true }`.
- `crates/core/Cargo.toml` - `iconv = ["protocol/iconv"]` and the
  default-on `default = ["zstd", "lz4", "xattr", "iconv"]`.
- Workspace `Cargo.toml:55` - `iconv = ["cli/iconv", "core/iconv"]`,
  default-enabled.

The feature is on by default. The dead-end is not a feature-gate
issue.

## 3. Gaps, by hot path

This section enumerates the four hot paths where the converter
*should* be applied but is not, with the exact source line at which
the producer call should be inserted.

### 3.1 Sender file-list emit on the local-copy path

**Status: dead.** SSH and daemon are wired (PR #3458). Local copy is
not.

The hook is at
`crates/protocol/src/flist/write/encoding.rs:308-327`
(`apply_encoding_conversion`). The producer site that activates it
is `crates/transfer/src/generator/mod.rs:564-565`:

```rust
if let Some(ref converter) = self.config.connection.iconv {
    writer = writer.with_iconv(converter.clone());
}
```

`self.config.connection.iconv` is populated for SSH and daemon
through `apply_common_server_flags`
(`crates/core/src/client/remote/flags.rs:228`), but the local-copy
path does not go through that function. Local copy uses the engine
crate (`crates/engine/`) rather than the transfer crate's generator;
repository-wide grep across `crates/engine/` for
`iconv|FilenameConverter|EncodingConverter` returns zero matches.
The engine therefore has no parallel field for the converter to land
on.

The local-copy entry point is
`crates/core/src/client/run/mod.rs:248`:

```rust
// Local copy path
let plan = match LocalCopyPlan::from_operands(config.transfer_args()) {
    Ok(plan) => plan,
    Err(error) => return Err(map_local_copy_error(error)),
};
```

followed by

```rust
// crates/core/src/client/run/mod.rs:274-275
let filter_program = filters::compile_filter_program(config.filter_rules())?;
let mut options = build_local_copy_options(&config, filter_program);
```

`build_local_copy_options(&config, filter_program)` reads recursion,
deletion, limits, bandwidth, compression, metadata, behavioural
flags, paths, time, reference dirs, and filter program from
`&ClientConfig`. It does **not** read `config.iconv()`. After the
call returns, `&config` is no longer threaded into the engine, so
the converter has no plumbing route. (`crates/core/src/client/run/mod.rs:585-589`
`pub fn build_local_copy_options(...)` and the
`LocalCopyOptionsBuilder::build` body at lines 351-583.)

The fix lives in this file. Either the engine's `LocalCopyOptions`
needs an `iconv: Option<FilenameConverter>` field with a setter
called from `build_local_copy_options`, or local copy needs to be
rerouted through the transfer crate's generator. The smaller change
is the former.

### 3.2 Receiver file-list ingest on the local-copy path

**Status: dead.** SSH and daemon are wired (PR #3458). Local copy is
not.

The hook is at `crates/protocol/src/flist/read/name.rs:101-118`
(`apply_encoding_conversion`). The producer site that activates it
is `crates/transfer/src/receiver/mod.rs:369-370`:

```rust
if let Some(ref converter) = self.config.connection.iconv {
    reader = reader.with_iconv(converter.clone());
}
```

The same `apply_common_server_flags` -> `connection.iconv` chain that
covers SSH and daemon does **not** apply on local copy. The
local-copy executor in `crates/engine/` does not traverse a
`FileListReader` produced by `crates/protocol/src/flist/read/`; it
walks the source tree directly. The receiver-flist-ingest code path
therefore is structurally absent on local copy. The fix is the
mirror of #3.1: either teach the engine to apply a converter when
matching or staging filenames, or reuse the protocol crate's flist
machinery on the local-copy side.

### 3.3 Filter-rule path matching

**Status: dead on every transport.** Repository-wide grep across
`crates/filters/` for `iconv|FilenameConverter|EncodingConverter`
returns zero matches.

- `crates/filters/src/chain.rs:204-218` - the docstring example for
  `FilterChain::new`. The constructor signature at
  `crates/filters/src/chain.rs:226` is
  `pub fn new(global: FilterSet) -> Self`. There is no
  `Option<FilenameConverter>` parameter and no `with_iconv` builder.
- Production call sites that build a `FilterChain`:
  - `crates/transfer/src/generator/filters.rs:53` -
    `self.filter_chain = FilterChain::new(filter_set);`.
  - `crates/transfer/src/generator/filters.rs:80` -
    `self.filter_chain = FilterChain::new(filter_set);`.
  - `crates/transfer/src/receiver/transfer.rs:895` -
    `let mut chain = FilterChain::new(filter_set);`.
- `crates/core/src/client/run/mod.rs:274` -
  `let filter_program = filters::compile_filter_program(config.filter_rules())?;`
  for the local-copy path. The compile function lives at
  `crates/core/src/client/run/filters.rs:14
  pub(crate) fn compile_filter_program(rules: ...)`. Neither
  signature accepts a converter.

The upstream invariant (`exclude.c`) is that filter-rule strings are
typed by the user in the local charset, and filenames are
post-`ic_recv`-converted before being matched against those rules.
oc-rsync today matches whatever bytes the engine hands the filter
chain, which on a transcoded session is the *remote* charset. User
patterns that contain non-ASCII bytes (or that match against
non-ASCII names) silently fail to match.

The fix is structural: extend `FilterChain::new` to accept an
`Option<FilenameConverter>` (preferably as a builder method to
preserve the existing call sites), and apply
`converter.remote_to_local()` to the candidate path inside
`FilterChain::allows()` before evaluating rules. Alternatively, wrap
the chain in an `IconvAwareFilterChain` that pre-converts the input
path. Either approach matches upstream's `exclude.c` invariant
without changing the wire format.

### 3.4 Daemon module config (`charset` directive)

**Status: parsed but never consumed.**

- `crates/daemon/src/daemon/sections/config_parsing/module_directives.rs:330-337` -
  the directive parser stores `charset = <value>` on the module
  builder.
- `crates/daemon/src/daemon/sections/module_definition/setters.rs:660-677` -
  validates uniqueness, stores the value.
- `crates/daemon/src/daemon/sections/module_definition/finish.rs:113` -
  `charset: self.charset.unwrap_or(None),` propagates the value to
  the resolved module definition.
- `crates/daemon/src/daemon/module_state/definition.rs:108-109` -
  `pub(crate) charset: Option<String>` on the per-module state.
- `crates/daemon/src/daemon/module_state/definition.rs:413-415` -
  `pub(crate) fn charset(&self) -> Option<&str>` accessor.

Repository-wide grep for callers of `definition.charset()`,
`module.charset()`, or `.charset()` (on the module-state type)
returns **only the test reference** at
`crates/daemon/src/daemon/sections/config_parsing/tests.rs:695`.
Production code never reads the resolved charset.

Upstream applies the per-module `charset` at
`target/interop/upstream-src/rsync-3.4.1/clientserver.c:712-717`:

```c
#ifdef ICONV_OPTION
    iconv_opt = lp_charset(i);
    if (*iconv_opt)
        setup_iconv();
    iconv_opt = NULL;
#endif
```

i.e., the daemon temporarily sets `iconv_opt` from the module
charset, calls `setup_iconv()`, then clears the global. The
client's own `--iconv` (forwarded in argv) takes precedence; the
daemon-side `charset` is the default when the client did not pass
an `--iconv` of its own.

The fix is to read the resolved module's `charset` in the daemon's
session-bringup path, parse it through `IconvSetting::parse`, run
`resolve_converter`, and write the result onto
`ServerConfig.connection.iconv` before the receiver / generator is
spawned. The exact splice point is in the daemon module-access
plumbing where the `ServerConfig` is built; see
`crates/daemon/src/daemon/sections/module_access/` and the daemon's
`ServerConfig` factories (out of scope for this audit but tracked
under #1917).

### 3.5 Adjacent inert sites (out of scope for #1918)

The following sites are also dead but are tracked under sibling
trackers and `docs/audits/iconv-pipeline.md`:

- Symlink targets, both directions
  (`crates/protocol/src/flist/read/extras.rs:25-66` and
  `crates/protocol/src/flist/write/encoding.rs:106` -
  `read_symlink_target` / `write_symlink_target` perform no
  conversion; no `sender_symlink_iconv` gate). See
  `docs/audits/iconv-pipeline.md` Findings 1 and 2.
- `--files-from` forwarding
  (`crates/protocol/src/files_from.rs` - zero iconv references).
- Secluded args and protected args
  (`crates/protocol/src/secluded_args.rs` - zero iconv references).
- `MSG_*` log-line transcoding (no producer in
  `crates/logging/`).
- `CF_SYMLINK_ICONV` advertisement in
  `crates/transfer/src/setup/capability.rs::build_capability_string`
  is unconditional, not gated on `IconvSetting::is_unspecified()`.

These do not block #1918; they are tracked alongside #1912, #1913,
and the broader pipeline gap inventory.

## 4. Wire-up plan, mapped to existing trackers

The fix decomposes naturally into the existing tracker chain. Each
tracker is independently mergeable. The dependency arrows below
encode the smallest constraints; tasks within a row can land in
parallel.

### 4.1 Tracker map

| Tracker | Scope | State today | Gap closed by |
|---|---|---|---|
| #1909 | Audit `IconvSetting` parse-path dead-end. | Closed (PR #3514, `iconv-parse-deadend.md`). | n/a |
| #1910 | Locate or design `FilenameConverter` trait. | Closed (PR #3517, `iconv-filename-converter-design.md`). The struct already exists at `crates/protocol/src/iconv/converter.rs:21`. | n/a |
| #1911 | Wire `IconvSetting -> FilenameConverter` at config build. | Partial (PR #3458 closed SSH and daemon via `apply_common_server_flags`; local copy remains). | The local-copy gap (3.1, 3.2). |
| #1912 | Apply on sender file-list emit. | Hook at `crates/protocol/src/flist/write/encoding.rs:312` is wired to a converter when one is present. Producer at `crates/transfer/src/generator/mod.rs:564` activates for SSH and daemon; local-copy needs the engine wiring. Symlink-target sub-gap (`flist.c:1609`) is separate. | (3.1) for local copy; symlink-target sub-gap tracked in `iconv-pipeline.md` Finding 2. |
| #1913 | Apply on receiver file-list ingest. | Hook at `crates/protocol/src/flist/read/name.rs:101` and producer at `crates/transfer/src/receiver/mod.rs:369` are wired for SSH and daemon. Symlink-target sub-gap (`flist.c:1141`) is separate. | (3.2) for local copy; symlink-target sub-gap tracked in `iconv-pipeline.md` Finding 1. |
| #1914 | Apply in filter-rule path matching. | Dead. Repo-wide grep across `crates/filters/` finds no iconv references. | (3.3). |
| #1915 | Hard-error `--iconv` when feature off. | Closed. `crates/cli/src/frontend/execution/options/iconv.rs:67-74` rejects with `rsync_error!(1, ...)` when `not(feature = "iconv")`. | n/a (already in tree). |
| #1916 | Interop test against upstream rsync 3.4.1. | Stub at `tools/ci/run_interop.sh:2980-3087` per `docs/audits/iconv-feature-design.md:170-188`. | (5) below. |
| #1917 | Daemon module config `iconv` / `charset` directive. | Dead. The directive parses but is unread (3.4). | (3.4). |
| #1918 | This audit. | Closes with this PR. | n/a. |
| #1919 | Golden byte tests for converted filenames. | Not started. Depends on #1912 and #1913 producing wire bytes that can be captured. | (5) below. |

### 4.2 Ordering

```
#1909 (closed) ─┐
#1910 (closed) ─┼─▶ #1911 ─┬─▶ #1912 ─┐
                            ├─▶ #1913 ─┼─▶ #1914 ─┬─▶ #1916
                            ├─▶ #1917 ─┘          └─▶ #1919
#1915 (closed) ─────────────┘
```

Smallest-PR-first sequence:

1. **#1911 local-copy bridge** - extend `LocalCopyOptions` (in
   `crates/engine/src/local_copy/`) with `iconv:
   Option<FilenameConverter>` and call
   `config.iconv().resolve_converter()` from
   `crates/core/src/client/run/mod.rs::build_local_copy_options`.
   Roughly +20 lines, no new types.
2. **#1912 / #1913 engine application** - apply the converter on
   the engine's source-walk and dest-write codepaths so non-ASCII
   names round-trip through local copy. Wire #1912 / #1913 in
   parallel; they touch different sides of the engine.
3. **#1917 daemon directive** - read
   `module.charset()` in the daemon's `ServerConfig` factory, run
   it through `IconvSetting::parse` then `resolve_converter`, and
   write the result onto `connection.iconv` before the
   receiver/generator is spawned.
4. **#1914 filter-chain converter** - thread
   `Option<FilenameConverter>` through `FilterChain::new` and apply
   it inside `FilterChain::allows()` on candidate names. Touches
   `crates/filters/`, `crates/transfer/src/{generator,receiver}/`,
   and `crates/core/src/client/run/filters.rs`.
5. **#1916 / #1919 tests** - interop and golden byte tests as
   described in section 5 below.

Tasks 1-3 are each smaller than 100 net source lines plus tests.
Task 4 is wider because it changes a public-ish API
(`FilterChain::new`) and is touched by multiple consumers; expect
~200-300 lines plus tests. Task 5 is test-only.

## 5. Test surface

The audit-level requirement is that every wired path has at least
one test that exercises the converter on real bytes, and that the
on-the-wire shape matches upstream rsync 3.4.1 byte for byte.

### 5.1 Interop tests (#1916)

Targets:

- Identity round-trip (`--iconv=UTF-8,UTF-8`) against upstream
  3.0.9, 3.1.3, 3.4.1, push and pull, daemon and SSH transports.
  Already stubbed at
  `tools/ci/run_interop.sh:2980-3087` per
  `docs/audits/iconv-feature-design.md:170-188`.
- Cross-charset (`--iconv=UTF-8,ISO-8859-1`) push and pull,
  round-tripping a non-ASCII filename and a non-ASCII symlink
  target. Verify wire bytes against a captured upstream trace.
- `--no-iconv` and `--iconv=-` against an upstream daemon with a
  `charset = utf-8` setting; confirm we override correctly per the
  daemon-default-but-user-overrides invariant
  (`target/interop/upstream-src/rsync-3.4.1/clientserver.c:712-716`).
- Lossy-conversion error: `--iconv=UTF-8,ASCII` with a path
  containing `é`; assert non-zero exit, role `[sender]`, exit code
  matching upstream's `RERR_PROTOCOL` / `RERR_UNSUPPORTED`.
- `--files-from` and `--secluded-args` cross-charset transfer
  (depends on #1912 / #1913 plus the iconv-pipeline Finding 3
  fixes).
- A pre-existing test at
  `crates/cli/src/frontend/tests/iconv.rs` confirms the CLI
  parser accepts the option; this is necessary but not sufficient.

### 5.2 Golden byte tests (#1919)

Add fixtures under `crates/protocol/tests/golden/` capturing the
wire bytes for `--iconv=UTF-8,ISO-8859-1` against a known input.
Compare against captured upstream traces obtained by running
`strace`/`tcpdump` against rsync 3.4.1 with the same input. The
existing protocol golden harness pattern is in
`crates/protocol/tests/` and is the canonical reference.

Required fixtures:

- File-list emit, identity (`--iconv=UTF-8,UTF-8`) - confirms the
  fast-path borrow.
- File-list emit, cross-charset - confirms the encoded payload.
- File-list ingest, both directions - confirms the round-trip
  `local_to_remote -> remote_to_local` is bit-identical to the
  source bytes when the charset round-trips losslessly.
- Filter-chain match decision on a transcoded name - confirms
  user-typed local-charset patterns match post-conversion names
  (depends on #1914).

### 5.3 Unit and property tests already in tree

- `crates/protocol/src/iconv/mod.rs:42-328` exercises identity,
  round-trip, lossy detection, alias normalization on the
  converter type.
- `crates/core/src/client/config/iconv.rs:168-301` exercises
  `IconvSetting::parse`, `IconvSetting::cli_value`, and
  `IconvSetting::resolve_converter` for every variant.
- `crates/cli/src/frontend/tests/iconv.rs` and
  `crates/cli/src/frontend/tests/parse_args_recognised_iconv.rs`
  exercise the CLI parser.
- `crates/core/src/client/remote/daemon_transfer/orchestration/tests.rs:694-770`
  exercises the SSH/daemon resolver bridge.

These cover the parse path and the converter type. They do not
exercise the local-copy path, the filter chain, or the daemon
`charset` directive. #1916 and #1919 are therefore distinct from the
existing tests.

## 6. Risks and decisions

### 6.1 Feature gating (#1915)

**Decision: closed.** Hard-error path in tree at
`crates/cli/src/frontend/execution/options/iconv.rs:67-74`: builds
with `--no-default-features` reject `--iconv=...` at config build
with `rsync_error!(1, "--iconv requires the iconv feature, which
was disabled at build time")`.

A complementary risk remains: the unknown-charset path inside
`IconvSetting::resolve_converter`
(`crates/core/src/client/config/iconv.rs:138-148`) emits a
`tracing::warn!` and returns `None`. If `tracing` is disabled the
user sees nothing. The recommended remediation (return
`Result<Option<FilenameConverter>, ConversionError>` and surface as
`rsync_error!`) is documented in
`docs/audits/iconv-filename-converter-design.md:558-605` and is not
required for #1918 to land.

### 6.2 Protocol-version compatibility

**Decision: no wire-protocol change required.** `--iconv` is
implemented inline at the file-list emit/ingest sites; the wire
framing (length-prefixed strings) is unchanged. The single
negotiated bit, `CF_SYMLINK_ICONV`
(`crates/protocol/src/compatibility/known.rs:21-23` -
`KnownCompatibilityFlag::SymlinkIconv`), already exists in the
compatibility-flags exchange and round-trips correctly against the
golden test at
`crates/protocol/tests/compatibility_flags.rs:70-77`.

What needs gating:

- Today `crates/transfer/src/setup/capability.rs::build_capability_string`
  emits `'s'` (the SYMLINK_ICONV capability character)
  unconditionally. Upstream gates it on
  `iconv_opt != NULL` per
  `target/interop/upstream-src/rsync-3.4.1/compat.c:716-718`. This
  is harmless to upstream peers (the OR on both ends still gates
  the actual symlink-target transcoding) but is a divergence from
  upstream wire output and a noise source for interop trace
  diffing. See `docs/audits/iconv-pipeline.md` Finding 5.
- `sender_symlink_iconv` (the AND of both peers' bits and local
  `iconv_opt`) is not yet derived in oc-rsync. This blocks
  symlink-target transcoding in either direction. Track under the
  symlink-target sub-gap in `iconv-pipeline.md` Findings 1 and 2.

Neither change is on the critical path for #1918's local-copy /
filter / daemon-directive wiring. They are recorded here so the
remediation does not break the SymlinkIconv golden test.

### 6.3 Error reporting on conversion failure

**Decision: asymmetric, mirror upstream.** The receiver-side and
sender-side conversion failure semantics differ in upstream. The
table at section 1.5 above is the canonical reference. The current
oc-rsync conversion methods both return `Result`, which lets the
call site choose policy. The wiring tasks (#1912 / #1913 / #1914 /
#1917) must apply the upstream-matching policy at each call site:

- Sender flist emit (`crates/protocol/src/flist/write/encoding.rs:312`):
  failure is fatal. Map to a hard `io::Error` with
  `io::ErrorKind::InvalidData`, propagate to the engine /
  generator, exit non-zero.
- Receiver flist ingest (`crates/protocol/src/flist/read/name.rs:106`):
  failure warns and drops the entry. Today the function returns
  `io::Error`; the upstream-matching change is to log + skip rather
  than propagate.
- Filter chain (#1914): failure is *match-deny* with a warning. A
  pattern that fails to convert against a candidate name should
  not match; a candidate name that fails to convert should be
  treated as if it did not pass the filter.
- Daemon `charset` directive (#1917): startup-time failure aborts
  the daemon with a config-parse error mirroring
  `target/interop/upstream-src/rsync-3.4.1/rsync.c:130-140`'s
  `RERR_UNSUPPORTED` exit.

### 6.4 Charset normalization rules

`encoding_rs` performs WHATWG-style label normalization (e.g.,
`utf8` -> `UTF-8`, `latin1` -> `windows-1252`). Upstream `iconv`
on glibc performs different normalizations and supports more
charsets (EBCDIC, rare CJK variants). The decision to stay on
`encoding_rs` is documented in
`docs/audits/iconv-feature-design.md:148-167`; the trade-off is
explicit failure (`rsync_error!`) on labels we do not recognise
versus silent mojibake on a system-iconv build that nominally
accepts the label but produces non-round-trippable bytes.

A user-visible consequence: oc-rsync rejects (or `tracing::warn!`s)
some labels that upstream accepts. This is a documented divergence
and is preferred to silent mojibake.

### 6.5 Locale detection

`crates/protocol/src/iconv/converter.rs:417-429`'s
`converter_from_locale()` currently returns `identity()` on both
feature configurations because we do not consult `LC_CTYPE`. This
matches the lowest-risk path (UTF-8 on UTF-8) which is correct on
modern Linux/macOS deployments but diverges from upstream
`target/interop/upstream-src/rsync-3.4.1/rsync.c:89` `default_charset()`
(which calls `nl_langinfo(CODESET)`). On a system with `LANG=C` or
a non-UTF-8 locale, oc-rsync's `--iconv=.` will silently behave as
identity rather than transcoding to/from the actual locale charset.
Track separately; this is not on the critical path for #1918.

### 6.6 Daemon `charset` precedence

Upstream `target/interop/upstream-src/rsync-3.4.1/clientserver.c:712-717`
sets `iconv_opt = lp_charset(i)` *before* invoking
`setup_iconv()`, but the user's `--iconv=...` (forwarded in argv)
overrides via the popt re-processing in `options.c`. The contract
is: client `--iconv=X` wins over server `charset = Y`; client
`--no-iconv` wins over both. oc-rsync's #1917 implementation must
reproduce this precedence: `module.charset()` is the *default*,
applied only when `IconvSetting::Unspecified` is on the wire.

### 6.7 Engine-vs-transfer architecture

Local copy uses the engine crate's executor, which has no `iconv`
plumbing today; SSH and daemon use the transfer crate, which is
fully plumbed. Two strategies:

- (A) Add `iconv: Option<FilenameConverter>` to `LocalCopyOptions`
  and apply it inside the engine's source-walk and dest-write
  paths. Smaller change; preserves the engine's zero-overhead
  local-copy fast path.
- (B) Reroute local copy through the transfer crate. Larger
  change; unifies both paths under one consumer.

Recommend (A). The engine's hot path is already tightly optimised
(sparse writes, buffer pool, parallel stat); rerouting via the
transfer crate adds overhead. The converter is `Cow::Borrowed` in
the identity case, so applying it on the engine path costs nothing
when `--iconv` is absent.

## 7. Recommendation

Land the four wirings smallest-PR-first, in this order:

1. **#1911 local-copy bridge** - the smallest change with the
   highest user-visible impact. A `LocalCopyOptions::iconv()`
   builder method and a single call in
   `build_local_copy_options(&config, ...)` close the gap on the
   most common path (local-to-local sync) for any user who passes
   `--iconv`.
2. **#1912 and #1913 engine application** - in parallel, apply the
   converter on the engine's source-walk and dest-write paths.
   Either through the file-list reader/writer (if the engine reuses
   the protocol crate's flist machinery internally) or through a
   thin engine-side hook on path output. Both depend on #1911.
3. **#1917 daemon directive** - read `module.charset()` in the
   daemon's `ServerConfig` factory, run it through
   `IconvSetting::parse` then `resolve_converter`, write to
   `connection.iconv` before the receiver / generator is spawned.
   Independent of #1911 / #1912 / #1913.
4. **#1914 filter-chain converter** - extend `FilterChain::new` to
   accept an `Option<FilenameConverter>`, thread through the three
   call sites, apply inside `FilterChain::allows()`. Larger
   surface; defer until 1-3 land so the test surface for filter
   matching is meaningful.
5. **#1916 / #1919 tests** - in parallel with #1914, capture the
   golden byte fixtures and the interop-against-3.4.1 cases.

Each of #1911 / #1912 / #1913 / #1917 is a candidate single-PR
landing. #1914 is the only one that touches a public-ish API
(`FilterChain::new`) and may need a deprecation cycle of its own;
the audit recommends a builder method (`with_iconv`) to preserve
the existing zero-arg signature. The full sequence is ~600-800
lines of production code plus tests, decomposable into 5-7 PRs.

The single largest mitigation against future drift is the golden
byte test under #1919: once the converter is producing bytes on the
wire, the golden trace pins them, and any later change that
silently breaks transcoding fails CI.

## 8. References

### 8.1 Sibling oc-rsync audits

- `docs/audits/iconv-parse-deadend.md` (PR #3514, tracker #1909)
  - parse-path trace from clap to `ClientConfig`.
- `docs/audits/iconv-filename-converter-design.md` (PR #3517,
  tracker #1910) - the converter type design and home crate
  decision.
- `docs/audits/iconv-pipeline.md` (PR #3424, tracker #1840) -
  per-call-site upstream coverage table.
- `docs/audits/iconv-feature-design.md` - cross-cutting design
  (library choice, capability advertisement, interop scope).

### 8.2 oc-rsync source (key entry points)

The full inventory of cited file:line locations lives inline in
sections 1-3 above. Quick index of the load-bearing entries:

- Bridge that activates the converter for SSH / daemon:
  `crates/core/src/client/remote/flags.rs:228`
  (`server_config.connection.iconv = config.iconv().resolve_converter();`).
- Bridge function:
  `crates/core/src/client/config/iconv.rs:130-151`
  (`IconvSetting::resolve_converter`).
- Local-copy entry where the bridge does **not** fire:
  `crates/core/src/client/run/mod.rs:274-275`
  (`compile_filter_program` and `build_local_copy_options`).
- Filter-chain constructor with no converter parameter:
  `crates/filters/src/chain.rs:226` (`FilterChain::new`).
- Daemon `charset` directive parser without a consumer:
  `crates/daemon/src/daemon/sections/config_parsing/module_directives.rs:330-337`,
  module-state field at
  `crates/daemon/src/daemon/module_state/definition.rs:108-109`.
- File-list emit / ingest hooks ready for a converter:
  `crates/protocol/src/flist/write/encoding.rs:308-327`,
  `crates/protocol/src/flist/read/name.rs:101-118`.
- Transfer-side consumer wiring:
  `crates/transfer/src/generator/mod.rs:564-565`,
  `crates/transfer/src/receiver/mod.rs:369-370`.
- CLI feature-gate hard error (#1915):
  `crates/cli/src/frontend/execution/options/iconv.rs:67-74`.

### 8.3 Upstream rsync 3.4.1

Source tree: `target/interop/upstream-src/rsync-3.4.1/`. Fetch
instructions live in the project conventions document.

- `options.c:219` - `iconv_opt` global.
- `options.c:814` - popt entry.
- `options.c:958, 995-1011, 1366, 1396, 1416, 1668, 2052-2058,
  2716-2722` - parse, daemon refuse-list interaction, defaults.
- `rsync.c:87-147` - `setup_iconv()` and `iconv_open` allocation.
- `rsync.c:179-281` - `iconvbufs()` core helper.
- `rsync.c:283-313` - `send_protected_args()` per-arg conversion.
- `flist.c:738-754, 1127-1150, 1579-1603, 1605-1621` - filename
  and symlink-target conversion sites.
- `io.c:416-452, 983-1031, 1240-1289, 1559-1591` -
  `--files-from`, `MSG_*`, `read_line`, `MSG_DELETED`
  conversions.
- `compat.c:716-718, 763-767` - `CF_SYMLINK_ICONV` advertisement
  and `sender_symlink_iconv` derivation.
- `clientserver.c:142, 712-717, 1175-1183` - daemon-side
  `setup_iconv` invocation, per-module `charset`, teardown.
- `loadparm.c` (per `daemon-parm.txt:19 STRING charset NULL`) -
  per-module charset directive.
- `rsync.1.md:557, 2421-2422, 3731-3755` - end-user
  documentation, including the daemon-overrides-charset behaviour.

### 8.4 Tracker entries

- #1909 (closed by `iconv-parse-deadend.md`).
- #1910 (closed by `iconv-filename-converter-design.md`).
- #1911 wiring at config build time (partial via PR #3458).
- #1912 sender flist emit (partial via PR #3458).
- #1913 receiver flist ingest (partial via PR #3458).
- #1914 filter-rule path matching.
- #1915 hard-error when feature off (closed in tree at
  `crates/cli/src/frontend/execution/options/iconv.rs:67-74`).
- #1916 interop test against upstream 3.4.1.
- #1917 daemon module config `charset` directive.
- #1918 (this audit).
- #1919 golden byte tests.

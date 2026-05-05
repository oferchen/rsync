# Audit: `--iconv` `FilenameConverter` - locate or design the bridge type

Tracking: oc-rsync task #1910.

## Headline finding

**A `FilenameConverter` already exists.** It lives at
`crates/protocol/src/iconv/converter.rs:21` and is re-exported from
`crates/protocol/src/iconv/mod.rs:33` and `crates/protocol/src/lib.rs:145`.
It is a concrete struct (not a trait), backed by `encoding_rs` behind the
`iconv` Cargo feature, with an identity stub when the feature is off. The
constructors (`new`, `identity`, `new_lenient`), the conversion helpers
(`local_to_remote`, `remote_to_local`, plus the higher-level string API
`to_local` / `to_remote`), and the bridge from `IconvSetting`
(`crates/core/src/client/config/iconv.rs:130 IconvSetting::resolve_converter`)
are all present and tested. Task #1910 therefore reduces to a
**confirm-and-document** task plus three minor refinements that this
audit recommends rather than mandates: rename the byte-oriented `Cow`
methods so the public API matches the tracker description
(`convert_send` / `convert_recv`), tighten the no-feature stub so a
`--iconv` request fails loudly rather than silently no-op'ing
(coordinated with #1915), and document the `Send + !Sync`-equivalent
guarantee that `encoding_rs::Encoding: 'static + Sync` already gives us
for free. The home crate (`crates/protocol/src/iconv/`) is the right
one. No new crate, no new trait abstraction, and no library change is
required.

## Companion audits

This audit is the design follow-on to two already-merged sibling docs.
Read them first.

- `docs/audits/iconv-parse-deadend.md` (PR #3514) - the eight typed
  hops `--iconv` walks from clap to `ClientConfig.iconv`, the SSH /
  daemon bridge that PR #3458 closed via `apply_common_server_flags`,
  and the two residual dead-ends on the local-copy and filter-rule
  paths.
- `docs/audits/iconv-inert.md` - the older pipeline gap inventory
  (largely superseded by `iconv-parse-deadend.md` for the producer
  side, but still authoritative for the file-list reader/writer hooks
  and the daemon module directive).
- `docs/audits/iconv-pipeline.md` - per-call-site upstream coverage
  table (every `iconvbufs` site mapped to its oc-rsync counterpart,
  with severity rollup).
- `docs/audits/iconv-feature-design.md` - cross-cutting design
  decisions (library choice, feature gating, capability advertisement),
  recommends staying on `encoding_rs`.

## Search results

Repo-wide grep for `FilenameConverter|EncodingConverter|IconvConverter`
returns matches in eight files. Test-only references are listed
separately.

### Production references to the converter type

- `crates/protocol/src/iconv/converter.rs:13` - `pub type
  EncodingConverter = FilenameConverter;` (the new string-based API
  is currently a type alias for the legacy byte-oriented API).
- `crates/protocol/src/iconv/converter.rs:21` - `pub struct
  FilenameConverter { ... }` definition. Carries
  `&'static encoding_rs::Encoding` for `local_encoding` and
  `remote_encoding` when the feature is on; an empty struct when off.
- `crates/protocol/src/iconv/converter.rs:46-50` - `Default for
  FilenameConverter` (returns `identity`).
- `crates/protocol/src/iconv/converter.rs:52-389` - `impl
  FilenameConverter` block: `identity`, `new`, `new_lenient`,
  `is_identity`, `remote_to_local`, `local_to_remote`,
  `local_encoding_name`, `remote_encoding_name`, `to_remote`,
  `to_local`. Each of the conversion methods has a feature-gated
  implementation and a feature-off stub.
- `crates/protocol/src/iconv/converter.rs:417-429` -
  `converter_from_locale()` free function (returns `identity()` on
  both feature configurations because we do not currently query
  `LC_CTYPE`).
- `crates/protocol/src/iconv/mod.rs:33` - `pub use converter::{
  EncodingConverter, FilenameConverter, converter_from_locale };`.
- `crates/protocol/src/lib.rs:145` - re-export at the crate root:
  `ConversionError, EncodingConverter, EncodingError, EncodingPair,
  FilenameConverter`.

### Production consumers (where the converter is stored or used)

- `crates/protocol/src/flist/read/mod.rs:27` - `use
  crate::iconv::FilenameConverter;`.
- `crates/protocol/src/flist/read/mod.rs:97` - `iconv:
  Option<FilenameConverter>` field on `FileListReader`.
- `crates/protocol/src/flist/read/mod.rs:339` -
  `FileListReader::with_iconv(mut self, converter: FilenameConverter)
  -> Self` setter.
- `crates/protocol/src/flist/read/mod.rs:628` - `let converted_name =
  self.apply_encoding_conversion(name)?;` invocation site inside the
  per-entry read loop.
- `crates/protocol/src/flist/read/name.rs:106-118` - per-entry
  `apply_encoding_conversion` body that calls
  `converter.remote_to_local(&name)`.
- `crates/protocol/src/flist/write/mod.rs:24` - `use
  crate::iconv::FilenameConverter;`.
- `crates/protocol/src/flist/write/mod.rs:94` - `iconv:
  Option<FilenameConverter>` field on `FileListWriter`.
- `crates/protocol/src/flist/write/mod.rs:304` -
  `FileListWriter::with_iconv(mut self, converter: FilenameConverter)
  -> Self` setter.
- `crates/protocol/src/flist/write/mod.rs:377` - `let name =
  self.apply_encoding_conversion(&raw_name)?;` invocation site inside
  `write_entry`.
- `crates/protocol/src/flist/write/encoding.rs:308-327` - per-entry
  `apply_encoding_conversion` body that calls
  `converter.local_to_remote(name)`.
- `crates/transfer/src/config/mod.rs:14` - `use
  protocol::FilenameConverter;`.
- `crates/transfer/src/config/mod.rs:101` - `pub iconv:
  Option<FilenameConverter>` field on `ConnectionConfig`.
- `crates/transfer/src/config/builder.rs:26` - `use
  protocol::FilenameConverter;`.
- `crates/transfer/src/config/builder.rs:184-187` -
  `ServerConfigBuilder::iconv(&mut self, converter:
  Option<FilenameConverter>) -> &mut Self`.
- `crates/transfer/src/receiver/mod.rs:369-370` - `if let Some(ref
  converter) = self.config.connection.iconv { reader =
  reader.with_iconv(converter.clone()); }`.
- `crates/transfer/src/generator/mod.rs:564-565` - mirror of the above
  on the writer.

### Bridge from `IconvSetting`

- `crates/core/src/client/config/iconv.rs:1` - `use
  protocol::iconv::{FilenameConverter, converter_from_locale};`.
- `crates/core/src/client/config/iconv.rs:130-151` -
  `IconvSetting::resolve_converter(&self) -> Option<FilenameConverter>`.
  Maps `Unspecified | Disabled` to `None`, `LocaleDefault` to
  `Some(converter_from_locale())`, `Explicit { local, remote }` to
  `FilenameConverter::new(local, remote_or_dot)` with a `tracing::warn!`
  fallback to `None` when `encoding_rs` does not recognise the label.
- `crates/core/src/client/remote/flags.rs:228` (PR #3458) -
  `server_config.connection.iconv =
  config.iconv().resolve_converter();` inside
  `apply_common_server_flags`. This is the only in-process call site
  that actually populates a `FilenameConverter` in production today.

### Cargo feature gating

- `Cargo.toml:29` - workspace `default-features` includes `iconv`.
- `Cargo.toml:55` - workspace alias `iconv = ["cli/iconv",
  "core/iconv"]`.
- `crates/protocol/Cargo.toml:27` - `iconv = ["encoding_rs"]`.
- `crates/protocol/Cargo.toml:38` - `encoding_rs = { version = "0.8",
  optional = true }`.
- `crates/core/Cargo.toml` - `iconv = ["protocol/iconv"]` and
  default-on inclusion.

### Test-only references

Tests in `crates/protocol/src/iconv/mod.rs:42-328`,
`crates/cli/src/frontend/tests/iconv.rs`,
`crates/cli/src/frontend/tests/parse_args_recognised_iconv.rs`,
`crates/core/src/client/remote/daemon_transfer/orchestration/tests.rs:694-770`.
None of these references suggest an alternate or competing converter
type; all exercise the existing `FilenameConverter`.

### What does **not** exist

Repo-wide grep for `IconvConverter` returns zero hits. Repo-wide grep
for `FilenameConverter|EncodingConverter|iconv` across `crates/filters/`
and `crates/engine/` returns zero hits. The local-copy executor and
the filter chain have no awareness of an `Option<FilenameConverter>`
today, exactly as `iconv-parse-deadend.md` recorded.

## Upstream behavior

References below are absolute paths inside the rsync 3.4.1 source
tree that ships with the interop harness
(`target/interop/upstream-src/rsync-3.4.1/`).

### Parse and setup

- `options.c:219` - `char *iconv_opt = ...` global.
- `options.c:814` - the popt entry `{"iconv", 0, POPT_ARG_STRING,
  &iconv_opt, 0, 0, 0}`.
- `options.c:1366` - `iconv_opt = strdup(arg)` on `--iconv=ARG`.
- `options.c:1396, 1416, 1668` - `iconv_opt = NULL` on `--no-iconv` and
  on parse failure.
- `options.c:2716-2722` - the local / remote split: `set =
  strchr(iconv_opt, ',');` (the upstream parser keeps the *local*
  side before the comma and the *remote* side after).
- `rsync.c:87-147` - `setup_iconv()` opens two `iconv_t` descriptors:
  `ic_send = iconv_open(UTF8_CHARSET, charset)` (i.e. the local-charset
  -> wire-charset direction is `local -> UTF-8`, where wire-charset is
  always UTF-8 in upstream's model) and `ic_recv = iconv_open(charset,
  UTF8_CHARSET)`. Both fail hard with `RERR_UNSUPPORTED` (exit code
  4) if `iconv_open` rejects the label.
- Note the upstream wire model: `ic_send` converts **local -> UTF-8**,
  `ic_recv` converts **UTF-8 -> local**. The "remote" charset is
  always UTF-8 on the wire; the user's `--iconv=UTF-8,ISO-8859-1`
  spec means the *remote peer* sees UTF-8 (which is the wire) and the
  local-side filesystem holds ISO-8859-1. oc-rsync's
  `FilenameConverter` collapses both directions into one struct that
  knows both labels and applies them, which is the correct
  abstraction; the upstream split into two `iconv_t` is an artefact
  of the C API.

### Per-call-site application

- `flist.c:738-754` - `recv_file_entry()` runs `iconvbufs(ic_recv,
  &inbuf, &outbuf, ICB_INIT)` on the just-decoded filename. On
  failure it prints `cannot convert filename: %s (%s)` via
  `FERROR_UTF8`, sets `outbuf.len = 0`, and continues with an empty
  thisname (the entry is then dropped by the empty-name check
  downstream). It does **not** silently fall back to raw bytes.
- `flist.c:1127-1150` - same shape on the symlink-target read,
  additionally gated on `sender_symlink_iconv`.
- `flist.c:1579-1603` - `send_file_entry()` runs `iconvbufs(ic_send,
  ...)` on the outgoing dirname/basename pair. Failure is fatal here
  because the wire format is broken if the sender cannot encode.
- `flist.c:1605-1621` - same on the symlink target, gated on
  `symlink_len && sender_symlink_iconv`.
- `io.c:416-452` - `forward_filesfrom_data()` per-record
  `iconvbufs(ic_send, ...)` for `--files-from`.
- `io.c:983-1031` - `send_msg()` text-message conversion for `MSG_*`
  payloads.
- `io.c:1240-1289` - `read_line()` with `RL_CONVERT` runs
  `iconvbufs(ic_recv, ...)` for daemon-side arg reading.
- `rsync.c:283-313` - `send_protected_args()` per-arg
  `iconvbufs(ic_send, ...)` with `ICB_EXPAND_OUT | ICB_INCLUDE_BAD |
  ICB_INCLUDE_INCOMPLETE | ICB_INIT`. Note the `ICB_INCLUDE_BAD` flag:
  here upstream **does** pass bad bytes through verbatim rather than
  drop them.
- `compat.c:716-718` - `CF_SYMLINK_ICONV` is set in local
  compatibility flags only when `iconv_opt != NULL`.
- `compat.c:763-767` - `sender_symlink_iconv` is true only when **both**
  the peer advertised `CF_SYMLINK_ICONV` and the local `iconv_opt`
  is non-null.

### Wire-side bytes

The wire format itself is not transformed: file-list entries carry
length-prefixed byte strings as before. The transformation happens at
the emit site (sender, post-`stat`) and the ingest site (receiver,
post-read). Filter-rule strings are typed locally by the user and
matched against post-`ic_recv` filenames; upstream does not run iconv
over filter rules themselves (`exclude.c` has no `iconvbufs` calls).

### Failure semantics summary

| Site | Direction | On lossy / invalid bytes |
|---|---|---|
| `flist.c:738` recv filename | wire -> local | warn + drop entry |
| `flist.c:1127` recv symlink | wire -> local | warn + drop target |
| `flist.c:1579, 1595` send filename | local -> wire | hard error, exit |
| `flist.c:1605` send symlink | local -> wire | hard error, exit |
| `rsync.c:304` protected args | local -> wire | include verbatim (`ICB_INCLUDE_BAD`) |
| `io.c:1025` `MSG_*` body | local -> wire | include verbatim |

The asymmetry matters: receiver-side conversion failures degrade to a
warning, sender-side conversion failures abort. oc-rsync's current
methods both return `Result`, which lets the caller pick policy; the
asymmetry must therefore live in the *call sites* (#1912 and #1913),
not in the trait.

## Three application sites that need a converter

All three are documented as gaps in `iconv-parse-deadend.md` and
`iconv-pipeline.md`. The trait design must serve all three without
bifurcation.

### Sender flist emit (`local -> wire`)

- Hook: `crates/protocol/src/flist/write/encoding.rs:312
  apply_encoding_conversion(&self, name: &'a [u8]) -> io::Result<Cow<'a,
  [u8]>>`.
- Producer: `crates/transfer/src/generator/mod.rs:564 if let Some(ref
  converter) = self.config.connection.iconv { writer =
  writer.with_iconv(converter.clone()); }`.
- Currently active for SSH and daemon (via PR #3458's
  `apply_common_server_flags`); inactive on the local-copy path
  because the engine has no parallel field. (#1912 follow-up.)
- Required signature shape: take a path or pre-flattened byte slice,
  return a `Cow<[u8]>` so the identity case borrows.

### Receiver flist ingest (`wire -> local`)

- Hook: `crates/protocol/src/flist/read/name.rs:106
  apply_encoding_conversion(&self, name: Vec<u8>) -> io::Result<Vec<u8>>`.
  (Note: this method takes an owned `Vec<u8>` and returns
  `Vec<u8>`, not `Cow<[u8]>` - a small inefficiency in the identity
  case that the redesign should fix.)
- Producer: `crates/transfer/src/receiver/mod.rs:369 if let Some(ref
  converter) = self.config.connection.iconv { reader =
  reader.with_iconv(converter.clone()); }`.
- Currently active for SSH and daemon; inactive on local-copy. (#1913
  follow-up.)
- Required signature shape: take a wire byte slice, return a
  `Cow<Path>` (or `Cow<[u8]>` cast to a `&Path` by the caller) so the
  receiver does not allocate when the bytes are valid local-charset
  already.

### Filter-rule path matching

- Filter chain construction: `crates/filters/src/chain.rs:204
  FilterChain::new(global)`.
- Caller sites:
  - `crates/transfer/src/generator/filters.rs:53,80
    self.filter_chain = FilterChain::new(filter_set);`.
  - `crates/transfer/src/receiver/transfer.rs:895 let mut chain =
    FilterChain::new(filter_set);`.
  - `crates/core/src/client/run/filters.rs:14
    compile_filter_program(rules)` (called from
    `crates/core/src/client/run/mod.rs:274` for the local-copy path).
- Currently the chain takes only filter specs and matches against
  whatever bytes the engine hands it. (#1914 follow-up.)
- Required signature shape: filter chain must hold an
  `Option<Arc<FilenameConverter>>` (or accept it on construction)
  and apply `convert_recv` to candidate names before matching, so
  user-typed include/exclude patterns (in local charset) match
  post-transcode names. Upstream relies on the same invariant in
  `exclude.c`: rules are typed locally and matched against
  already-`ic_recv`-converted filenames.

## Trait design proposal

The tracker text suggests a `FilenameConverter` trait with
`convert_send(&self, &Path) -> Cow<[u8]>` and `convert_recv(&self,
&[u8]) -> Cow<Path>`. The current `FilenameConverter` is a struct
with `local_to_remote(&[u8]) -> Result<Cow<[u8]>>` and
`remote_to_local(&[u8]) -> Result<Cow<[u8]>>`. The two designs differ
on three axes: trait-vs-struct, `Path` boundary in the public
signature, and `Result` vs infallible. This section recommends a
**concrete struct**, keeps the public method names aligned with the
tracker description (`convert_send` / `convert_recv`), promotes
`Path` to the public signature where it makes sense, and retains the
`Result` for both directions.

### Recommendation: keep the struct, rename and expand the public API

Trait abstractions are warranted when there are multiple
implementations. There is exactly one production conversion engine
(`encoding_rs`), one identity stub (when the `iconv` feature is off),
and the identity stub is shape-equivalent to the active impl. A
trait would only add a vtable hop and a `Box<dyn ...>` allocation per
session for no behavioural benefit. The struct stays.

The redesigned **public** API:

```rust
// crates/protocol/src/iconv/converter.rs
pub struct FilenameConverter { /* unchanged */ }

impl FilenameConverter {
    pub fn identity() -> Self;                                      // unchanged
    pub fn new(local: &str, remote: &str) -> Result<Self, ConversionError>;
    pub fn new_lenient(local: &str, remote: &str) -> Self;          // unchanged
    pub fn is_identity(&self) -> bool;                              // unchanged
    pub fn local_encoding_name(&self) -> &'static str;              // unchanged
    pub fn remote_encoding_name(&self) -> &'static str;             // unchanged

    /// Sender: local-charset path -> wire-charset bytes.
    pub fn convert_send<'a>(
        &self,
        path: &'a Path,
    ) -> Result<Cow<'a, [u8]>, ConversionError>;

    /// Receiver: wire-charset bytes -> local-charset path.
    pub fn convert_recv<'a>(
        &self,
        bytes: &'a [u8],
    ) -> Result<Cow<'a, Path>, ConversionError>;

    // Retain the byte-oriented helpers for internal callers
    // (flist write/read already use these names; keep them as
    // pub(crate) thin wrappers around the new path-typed methods).
    pub(crate) fn local_to_remote<'a>(&self, bytes: &'a [u8])
        -> Result<Cow<'a, [u8]>, ConversionError>;
    pub(crate) fn remote_to_local<'a>(&self, bytes: &'a [u8])
        -> Result<Cow<'a, [u8]>, ConversionError>;
}
```

#### Why `Cow<[u8]>` on send

The identity case (local == remote charset, or `iconv` feature off) is
the common case in production. `Cow::Borrowed` lets us avoid an
allocation per file entry in that case, matching upstream rsync's
`if (ic_send == (iconv_t)-1) { write_buf(fd, args[i], strlen(...) +
1); }` skip-the-buffer fast path
(`rsync.c:311-312`). When conversion is needed we own the encoded
bytes; `encoding_rs::Encoding::encode` already returns
`Cow<[u8]>` so the conversion implementation passes its `Cow`
through without re-allocating.

#### Why `&Path` input on send

The CLI and engine produce `&Path` natively; flattening to
`&[u8]` is a single `OsStr::as_encoded_bytes()` call but pushing
the `Path -> [u8]` flattening down into the converter avoids
duplicating it at every call site (sender flist emit, filter
matching, files-from forwarding, secluded-args). On Windows,
`Path` is UTF-16 internally; the converter is responsible for
the WTF-8 round-trip via `as_encoded_bytes`. This isolates the
platform-specific path semantics in one place.

#### Why `Cow<Path>` on recv

The receiver wants a `&Path` to pass to filesystem syscalls.
Constructing a `Path` from `&[u8]` on POSIX is zero-copy via
`OsStr::from_bytes`; on Windows it requires WTF-8 -> UTF-16 which
allocates. `Cow<Path>` lets the POSIX identity case stay borrowed
and reflects the Windows allocation cost honestly. The current
`apply_encoding_conversion` returns `Vec<u8>` and forces the caller
to reconstruct a `PathBuf`; the new shape moves that logic inside
the converter where it can elide the allocation when bytes are
already valid local-charset.

#### Why `Result`, and what error type

`Result<_, ConversionError>` matches the existing API and lets the
call site decide policy: sender hard-errors (`flist.c:1579-1603`),
receiver warns and drops (`flist.c:738-754`), protected-args includes
verbatim (`rsync.c:283-313` with `ICB_INCLUDE_BAD`). `ConversionError`
already exists at `crates/protocol/src/iconv/error.rs` and carries
both a message and the offending bytes
(`ConversionError::with_bytes`); that is sufficient. Do not introduce
a panic path: filenames originate from untrusted peers and a panic on
malformed input is a remote DoS. Do not introduce a sentinel value
(empty path) because `Path` has no canonical "empty"; the
`Result::Err` arm is explicit about the failure.

#### `Send + Sync` bound

`encoding_rs::Encoding` is `&'static` and the crate documents `Sync`.
`FilenameConverter` therefore is naturally `Send + Sync` with no
interior mutability. The struct holds two `&'static
encoding_rs::Encoding` references; cloning is `Copy`-cheap; sharing
across rayon worker threads is free. There is **no** need for the
glibc-iconv-style "thread-unsafe descriptor in a `Mutex`" pattern
that the tracker's `Send + !Sync` discussion would imply.

This is the single biggest reason to stay on `encoding_rs`. A
system-iconv backend would need either per-thread converters (giving
up the `&'static` cheapness) or a `Mutex`-wrapped `iconv_t` with the
attendant lock contention on every filename. The current backing
side-steps both problems.

#### `Clone`

The struct should remain `Clone` (and could be `Copy` once the
`encoding_rs` references stay `&'static`). `Clone` is required because
`FileListReader::with_iconv` and `FileListWriter::with_iconv` each take
the converter by value; `transfer::ConnectionConfig.iconv` is the
shared owner and clones into both sides. Filter-chain wiring (#1914)
will want a third clone. `Arc<FilenameConverter>` is unnecessary as
long as the struct stays cheap-Clone, which it is.

### Why not a trait

Three reasons.

1. **Single implementation.** There is no second backend planned. The
   `iconv-feature-design.md` library-choice table explicitly recommends
   staying on `encoding_rs`.
2. **Stub equivalence.** When the `iconv` feature is off, the struct
   becomes empty and methods become identity. A trait would force
   either `Box<dyn FilenameConverter>` (vtable per-call cost on the
   hot file-list emit path) or a generic parameter that bubbles up
   through `FileListReader<W: FilenameConverter>`, `FileListWriter<...>`,
   `ConnectionConfig<...>`, etc., forcing every caller to monomorphise
   and dragging the type parameter into public API. Neither is
   justified.
3. **Closure-style polymorphism is unwanted.** Per upstream, the
   converter is configured once per session from `iconv_opt` and
   never swapped. There is no use case for a per-call dynamic
   dispatch.

If a future second backend (say, `unic-locale` or system iconv via a
safe wrapper) is required, the current struct can be promoted to a
`pub trait FilenameConverter` with the existing struct renamed to
`EncodingRsFilenameConverter` and made the default. That migration is
mechanical and does not need to be paid for upfront.

## Backing implementation

Recommendation: stay on `encoding_rs`. Detailed rationale lives in
`iconv-feature-design.md:148-167` ("Library choice"). Summary:

| Property | `encoding_rs` (current) | system iconv | `iconv` crate |
|---|---|---|---|
| License | MIT/Apache | system | MIT |
| Maintenance | Mozilla (Firefox), active | platform vendors | low traffic |
| Cross-platform | yes (pure Rust) | requires libiconv on musl/Windows | requires libiconv on musl/Windows |
| Charset coverage | WHATWG-complete (UTF-8, ISO-8859, Windows-125x, EUC, Shift_JIS, GB18030, Big5, KOI8-R, etc.) | maximum (incl. EBCDIC, rare CJK variants) | matches system iconv |
| `unsafe` requirements | none | FFI only | wrapped FFI |
| Thread-safe converter | `&'static Encoding`: free `Sync` | `iconv_t`: not safe across threads in glibc | inherits system iconv constraints |
| Already a dependency | yes | no | no |

The project's unsafe-code policy forbids unsafe in `protocol`. A direct
`iconv` FFI binding is therefore not an
option. The `iconv` Rust crate wraps `libiconv` with a safer API but
still requires linking system iconv on Windows (via `win-iconv`)
and on musl-static builds. `encoding_rs` is pure Rust, ships with
zero system requirements, and covers every charset oc-rsync's
interop test plan exercises (`tools/ci/run_interop.sh:2980-3087`).

The narrow gap (no EBCDIC, no extremely rare CJK variants) is
acceptable: oc-rsync's failure mode for an unknown label is a clear
error from `FilenameConverter::new`, which we surface to the user
as `--iconv: unsupported charset`. That is preferable to silent
mojibake on a system iconv that nominally accepts the label but
produces non-round-trippable bytes.

## Home crate

The trait (struct, given the recommendation above) **stays at
`crates/protocol/src/iconv/`** with the existing module layout:

- `crates/protocol/src/iconv/mod.rs` - module entry, re-exports.
- `crates/protocol/src/iconv/converter.rs` - the struct and its
  methods.
- `crates/protocol/src/iconv/error.rs` - `ConversionError`,
  `EncodingError`.
- `crates/protocol/src/iconv/pair.rs` - `EncodingPair` (the
  charset-name pair value object).

Justification:

1. **Co-location with hooks.** The two production consumers
   (`FileListReader::apply_encoding_conversion`,
   `FileListWriter::apply_encoding_conversion`) are in the same crate.
   Putting the converter in `protocol` lets the conversion stay
   `pub(crate)` for the byte-oriented helpers and `pub` only at the
   boundary the transfer crate needs.
2. **No upward dep.** `crates/filters/` and `crates/core/` already
   depend on `crates/protocol/`. Moving the converter to a new crate
   or to `crates/core/` would either force a new crate (overkill for
   one struct) or invert the dep graph (`protocol -> core` is not
   allowed; the existing direction is `core -> protocol`).
3. **Optional dependency lives here.** The `encoding_rs` crate is
   `optional = true` on `crates/protocol/Cargo.toml:38`. Moving the
   converter elsewhere would force the optional dep to migrate too
   and complicate the workspace `iconv = ["cli/iconv", "core/iconv"]`
   feature plumbing.

Alternatives considered and rejected:

- **`crates/filters/`**: too narrow. The filter chain is a *consumer*
  of the converter; it does not own the conversion logic. Would also
  force `filters -> encoding_rs`, which `crates/filters/Cargo.toml`
  currently does not need.
- **`crates/core/`**: closer to the `IconvSetting` value type, but
  `core` depends on `protocol`, and the converter is needed by both
  `protocol` (for flist hooks) and `transfer` (for connection
  config). Putting it in `core` forces a downward dep that does not
  exist today.
- **A new `crates/iconv/` crate**: would add a workspace member for
  one struct and a 32-line error file. Premature factoring.

The existing layout (struct in `protocol`, setting in `core`, bridge
on `IconvSetting::resolve_converter`) is correct.

## `#[cfg(feature = "iconv")]` gating

The current state:

- `crates/protocol/src/iconv/converter.rs:22-26, 86-101, 105-121,
  126-139` has separate `#[cfg(feature = "iconv")]` and
  `#[cfg(not(feature = "iconv"))]` blocks for the struct fields,
  `new`, `new_lenient`, and the conversion methods.
- When the feature is off, `FilenameConverter::new` accepts only
  UTF-8 (or the empty/`.` aliases) and returns an
  `Err(ConversionError)` for any other label
  (`converter.rs:104-121`).
- `IconvSetting::resolve_converter` swallows that error
  (`crates/core/src/client/config/iconv.rs:138-148`) and emits a
  `tracing::warn!` then returns `None`. Effect: `--iconv=UTF-8,LATIN1`
  with `--no-default-features` silently no-ops the conversion.

This is inconsistent with task #1915 (`--iconv: reject with hard error
when iconv feature is off`). The recommended resolution:

1. **Hard error at config build.** When the `iconv` feature is off
   and the user supplied `--iconv=<anything>` (i.e.
   `IconvSetting::Explicit | IconvSetting::LocaleDefault`), fail at
   `crates/cli/src/frontend/execution/options/iconv.rs::accept_parsed_setting`
   (already present, see #1915 in progress) **before** the parse
   path even reaches `ClientConfig`. `iconv-parse-deadend.md`
   step 5 already documents the existing rejection at
   `accept_parsed_setting:60-74`; #1915 hardens it.
2. **Identity stub stays.** When the feature is off and no
   `--iconv` was supplied (`IconvSetting::Unspecified |
   IconvSetting::Disabled`), `resolve_converter` returns `None`.
   The `with_iconv` setter is never called, the
   `apply_encoding_conversion` hook short-circuits to
   `Cow::Borrowed`, and the binary behaves exactly as if iconv had
   never been compiled in.
3. **Silent fallback removed.** The `Err(_)` arm of
   `resolve_converter` should not silently emit `None` for an
   unsupported charset label even with the feature on. Today the
   `tracing::warn!` is the only visible signal; users without
   `tracing` enabled see nothing. Recommendation: change the return
   type to `Result<Option<FilenameConverter>, ConversionError>` and
   propagate the error as an `rsync_error!(1, ...)` at the call site
   in `apply_common_server_flags`. This aligns with
   `iconv-feature-design.md:138-141` ("emit a single
   `rsync_error!(1, ...)` at config build time, not a silent
   no-op") and converts the current `tracing::warn!` into a
   user-visible diagnostic.

## Wiring plan: open follow-up trackers

Closing #1910 (this audit) unblocks the following trackers:

- **#1911** - `wire IconvSetting -> FilenameConverter at config
  build`. Status: in progress (PR #3458 landed the SSH/daemon side
  via `apply_common_server_flags`; the local-copy path remains
  dead per `iconv-parse-deadend.md` summary table row 10). The trait
  shape this audit recommends is the contract the wiring depends on.
- **#1912** - `apply on sender file-list emit`. Hook is at
  `crates/protocol/src/flist/write/encoding.rs:312`. Producer at
  `crates/transfer/src/generator/mod.rs:564`. Activates once #1911
  closes the local-copy gap. Symlink-target sub-gap (`flist.c:1605`)
  remains; track in `iconv-pipeline.md` Finding 2.
- **#1913** - `apply on receiver file-list ingest`. Hook at
  `crates/protocol/src/flist/read/name.rs:101`. Producer at
  `crates/transfer/src/receiver/mod.rs:369`. Symmetric to #1912.
  Symlink-target sub-gap (`flist.c:1127`) tracked in
  `iconv-pipeline.md` Finding 1.
- **#1914** - `apply in filter-rule path matching`. Filter chain at
  `crates/filters/src/chain.rs:204`. Caller sites at
  `crates/transfer/src/{generator/filters.rs:53,80,
  receiver/transfer.rs:895}` and `crates/core/src/client/run/mod.rs:274`
  (local-copy). Requires extending `FilterChain::new` to take
  `Option<FilenameConverter>` (or wrapping the chain in a
  `IconvAwareFilterChain` that pre-converts candidate names).
- **#1916** - `add interop test against upstream rsync 3.4.1`.
  Targets in `tools/ci/run_interop.sh:2980-3087` (already partially
  stubbed). Add cross-charset push and pull with non-ASCII filename
  and symlink target, golden wire trace.
- **#1917** - `daemon module config iconv directive must take
  effect`. Add `iconv = <charset>` per-module directive to
  `crates/daemon/src/daemon/sections/module_parsing.rs` (currently
  only `iconv` and `no-iconv` appear in `REFUSABLE_OPTIONS`,
  module_parsing.rs:33-34). Mirror upstream `loadparm.c` /
  `daemon-parm.txt:19 STRING charset NULL`.
- **#1918** - `docs/audits/iconv-inert.md audit report`. Already
  committed as the parent audit; closing #1910 is the doc-side
  precondition for closing #1918.
- **#1919** - `golden byte tests for converted filename wire
  encoding`. Add fixtures under `crates/protocol/tests/golden/`
  comparing wire bytes for `--iconv=UTF-8,ISO-8859-1` against captured
  upstream traces. Depends on #1912 and #1913 landing first.

Order: #1910 (this) -> #1911 (the bridge gap closures) -> #1912 +
#1913 in parallel -> #1914 -> #1916 + #1917 + #1919 in any order.
#1915 is independent and can land before, after, or alongside any of
the above.

## References

### Sibling audits

- `docs/audits/iconv-parse-deadend.md` (PR #3514).
- `docs/audits/iconv-inert.md` (older, partially superseded).
- `docs/audits/iconv-pipeline.md` (per-call-site coverage).
- `docs/audits/iconv-feature-design.md` (cross-cutting design).

### oc-rsync source

- `crates/protocol/src/iconv/{mod,converter,error,pair}.rs` - the
  type definitions.
- `crates/protocol/src/lib.rs:145` - crate-root re-export.
- `crates/core/src/client/config/iconv.rs` - `IconvSetting` and
  `resolve_converter` bridge.
- `crates/core/src/client/remote/flags.rs:228` - sole production
  bridge call site (PR #3458).
- `crates/protocol/src/flist/{read/name.rs,write/encoding.rs}` -
  per-entry conversion hooks.
- `crates/protocol/src/flist/{read/mod.rs:339,write/mod.rs:304}` -
  `with_iconv` setters.
- `crates/transfer/src/config/{mod.rs:101,builder.rs:184}` -
  `ConnectionConfig.iconv` storage and setter.
- `crates/transfer/src/{generator/mod.rs:564,receiver/mod.rs:369}` -
  per-session producer sites.
- `crates/filters/src/chain.rs:204` - filter chain constructor (no
  iconv awareness yet).

### Upstream rsync 3.4.1

- `target/interop/upstream-src/rsync-3.4.1/options.c:219, 814,
  1366, 1396, 1416, 1668, 2052-2058, 2716-2722` - parser, defaults,
  refuse-list interaction.
- `target/interop/upstream-src/rsync-3.4.1/rsync.c:87-147` -
  `setup_iconv()` and `iconv_open` allocation.
- `target/interop/upstream-src/rsync-3.4.1/rsync.c:179-281` -
  `iconvbufs()` core helper.
- `target/interop/upstream-src/rsync-3.4.1/rsync.c:283-313` -
  `send_protected_args()` per-arg conversion.
- `target/interop/upstream-src/rsync-3.4.1/flist.c:738-754,
  1127-1150, 1579-1603, 1605-1621` - filename and symlink-target
  conversion sites.
- `target/interop/upstream-src/rsync-3.4.1/io.c:416-452, 983-1031,
  1240-1289` - `--files-from`, `MSG_*`, `read_line` conversions.
- `target/interop/upstream-src/rsync-3.4.1/log.c:251-371` - log-line
  transcoding via `ic_chck` / `ic_recv`.
- `target/interop/upstream-src/rsync-3.4.1/compat.c:716-718,
  763-767` - `CF_SYMLINK_ICONV` advertisement and
  `sender_symlink_iconv` derivation.
- `target/interop/upstream-src/rsync-3.4.1/clientserver.c:142, 715` -
  daemon-side `setup_iconv()` invocation.
- `target/interop/upstream-src/rsync-3.4.1/main.c:638, 648, 653,
  1820` - non-daemon `setup_iconv()` invocation.
- `target/interop/upstream-src/rsync-3.4.1/daemon-parm.txt:19
  STRING charset NULL` - per-module charset directive.
- `target/interop/upstream-src/rsync-3.4.1/rsync.1.md:557,
  2421-2422, 3731-3755` - end-user documentation, including the
  daemon-overrides-charset behaviour.

### Tracker entries

- #1909 (closed by `iconv-parse-deadend.md`).
- #1910 (this audit).
- #1911, #1912, #1913, #1914 (wiring gaps; unblocked by this audit).
- #1915 (hard-error when feature off; in progress).
- #1916 (interop test).
- #1917 (daemon `iconv` directive).
- #1918 (closed by `iconv-inert.md`).
- #1919 (golden byte tests).

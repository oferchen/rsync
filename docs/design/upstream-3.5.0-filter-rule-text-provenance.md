# Upstream rsync 3.5.0 filter-rule text provenance

Read-only extraction of the rule set oc-rsync must mirror. Source of truth is
the C at `target/interop/upstream-src/rsync-3.5.0/`, primarily `exclude.c`
plus the `FILTRULE_FROM_FILE` bit in `rsync.h`.

This document is the specification the implementation work is measured against.
It records what upstream *does*, not what oc-rsync currently does. Every rule
below was additionally confirmed by running the real 3.5.0 binary; the probes
are in section 9.

## 0. Why this exists

3.5.0 changed what a filter diagnostic is allowed to say. The reasoning is
stated at `exclude.c:49-56`:

> A rule that fails to parse used to be echoed back verbatim, and the peer
> chooses which file gets merged (a per-directory merge rule travels over the
> protocol, so no argument of ours ever names it), which made the filter parser
> a read-any-line oracle: any line that is not valid filter syntax came straight
> back in the error. Report where the bad rule is, not what it says.

Two consequences shape the whole design:

- The rule is **not** "redact everything". Text that came from the operator's
  own argument is returned unchanged, "because it is the user's own and hiding
  it only makes typos harder to fix" (`exclude.c:90-95`). Only text that came
  out of a **file's contents** is replaced.
- The decision is made at **one chokepoint**, not at each message. The comment
  at `exclude.c:96-98` gives the reason: "Doing it here rather than at each site
  is the point: a message added later cannot reintroduce the leak by forgetting
  to check, and there is one place to audit."

An implementation that patches the messages individually satisfies the letter
and loses the property.

## 1. The provenance predicate

`exclude.c:67-69`:

```c
#define TEXT_FROM_FILE(template) \
    (rule_src_in_file \
     || ((template) && (template)->rflags & FILTRULE_FROM_FILE))
```

Two inputs, and both are load-bearing:

| Input | Kind | Answers |
|---|---|---|
| `rule_src_in_file` | dynamic, process-global | are we parsing a file's contents *right now*? |
| `FILTRULE_FROM_FILE` (`rsync.h:1055`, bit 21) | per-rule flag | did *this rule's* text come from a file? |

The per-rule flag exists for the **deferred** case: a per-directory merge rule
whose name came from a file, but which is applied during traversal, long after
that file was read and closed. At that moment `rule_src_in_file` is 0. A single
dynamic flag cannot express it and would print the raw text.

Four module-globals carry the state (`exclude.c:56-62`):

| Global | Meaning |
|---|---|
| `rule_src_in_file` | parsing a file's contents right now |
| `rule_src_file` | the file's name, **when that name is itself safe to show** |
| `rule_src_line` | current line, or `-1` when the count is not a line count |
| `rule_src_named_at` | where a file whose own name must not be printed was named |

`rule_src_file` being `NULL` while `rule_src_in_file` is 1 is a normal state,
not an error: it means we are in a file whose *name* is also peer-controlled.

## 2. Two helpers, not one

`rule_text_len` (`exclude.c:102-118`) replaces the **text**:

```c
if (!TEXT_FROM_FILE(template)) {
    if (len < 0) return text;
    snprintf(b, sizeof buf[0], "%.*s", len, text);
    return b;
}
snprintf(b, sizeof buf[0], "<rule from %s>", rule_src_where());
```

`rule_detail` (`exclude.c:127-131`) drops the **detail about** the text:

```c
return TEXT_FROM_FILE(template) ? "" : detail;
```

Its comment: "For the extra detail some messages add ABOUT the text - a
character of it, an offset into it. Dropped along with the text it describes."

This second helper is easy to miss and it matters more than it looks. A message
of the form `invalid modifier 'k' at position 1` leaks the file's contents one
byte at a time, which is a finer-grained oracle than echoing the whole line.
Redacting only the text leaves the oracle intact.

### The buffer is rotated, and that is required

```c
static char buf[2][BIGPATHBUFLEN];
static int which = 0;
which ^= 1;
```

"The returned buffer is rotated, so two calls in one rprintf() are safe."
Five sites call a helper twice in a single message: `:272-274`, `:1101-1102`,
`:1377-1378`, `:1659-1660`, `:1699-1700`. A single-buffer port corrupts all of
them, and the corruption is silent - the message still prints, with one
fragment showing the other's text.

## 3. `rule_src_where()` has four arms

`exclude.c:72-86`:

| Condition | Result |
|---|---|
| `!rule_src_file && !rule_src_named_at` | `"a file read earlier"` |
| `!rule_src_file` | `"a file named at %s"` (`rule_src_named_at`) |
| `rule_src_line < 0` | `rule_src_file` alone |
| otherwise | `"%s line %d"` |

The two degraded arms are not defensive padding. They are the normal output
whenever the merge file's own name came from a file (arm 2) or whenever the
rule is a deferred per-dir merge (arm 1).

Line numbers count **physical lines**, including comments and blank lines - the
counter increments once per read iteration (`exclude.c:1759-1760`), before the
comment and empty-token filter at `:1806`. Numbering only the rules that parse
produces different, wrong numbers.

## 4. Where provenance is set

Exactly two sites set `FILTRULE_FROM_FILE`:

- `exclude.c:1265` - `if (rule_src_in_file) rule->rflags |= FILTRULE_FROM_FILE;`
  The trailing comment pins the ordering: "before parse_merge_name()".
- `exclude.c:1567` - the synthetic exclude-self rule inherits it:
  `excl_self->rflags = rule->rflags & FILTRULE_FROM_FILE;`

The second is the one an independent implementation will omit. Its comment
records the bug that omitting it caused: the pattern of an exclude-self rule is
the merge rule's own text, so "built by hand, this rule looked argument-origin
once parsing finished and the match trace echoed a merge file's contents at
-vv". A synthetic rule constructed locally can still carry text that came from
a file. Provenance must be inherited, not inferred from how the rule was built.

`FILTRULE_FROM_FILE` never reaches the wire. `get_rule_prefix` tests only
`FILTRULES_SIDES`, `FILTRULE_DIRECTORY`, `FILTRULE_ABS_PATH`,
`FILTRULE_MERGE_FILE` and `FILTRULE_INCLUDE`, so bit 21 is purely local state
with no golden-byte or interop exposure.

## 5. Nesting: save, restore, and snapshot

A merge rule inside a merge file re-enters `parse_filter_file`, so all four
globals are saved on entry (`:1734-1739`) and restored on exit (`:1814-1816`).

Three details are easy to lose:

- `named_at` is `strlcpy`'d from `rule_src_where()` **before** the state is
  overwritten, precisely because that function returns a static buffer
  (`:1744-1749`).
- A deferred merge gets `rule_src_named_at = NULL` deliberately - it has "no
  live location to point at", so upstream leaves the generic description rather
  than nesting two vague ones.
- `rule_src_file` is re-set inside the read loop at `:1807`, before each
  `parse_filter_str`, because a nested merge on an earlier line has clobbered it.

`rule_src_line = word_split ? -1 : 0` (`:1754`): a word-split merge file yields
tokens, not lines, so line counting is suppressed rather than reported wrongly.

## 6. The redaction has two channels

Text is one. **errno is the other.**

On a failed open (`:1705-1717`), upstream branches:

```c
if (TEXT_FROM_FILE(template)) {
    /* errno too: it answers "does this path exist". */
    rprintf(FERROR, "failed to open %sclude file %s\n", ..., rule_text(template, fname));
} else {
    rsyserr(FERROR, errno, "failed to open %sclude file %s", ..., fname);
}
```

`rsyserr` appends `strerror(errno)`; the from-file arm uses plain `rprintf` and
drops it. An implementation that redacts the name but keeps "No such file or
directory" has rebuilt a path-existence oracle.

Related, at `:1753`: `rule_src_file = named_by_file ? NULL : src_name`. When a
rule we read named *this* file, our own path is file content too. Upstream does
not print the name in a shortened or relative form - it withholds the name
entirely and falls back to arm 2 of section 3.

At `:1800-1801`, an over-long line is discarded with `rule_text_len(NULL, line, 0)`
- length zero. Not even a truncated prefix is echoed.

## 7. The call sites

Fourteen messages, eighteen helper calls, all in `exclude.c`:

| Line | Message |
|---|---|
| 135 | `filter_rule_err` - the generic syntax-error emitter |
| 272-274 | `add_rule` debug trace (2 x `rule_detail`, 1 x `rule_text_len`) |
| 379 | per-dir `debug_type` label |
| 728 | merge-file name |
| 742 | filter filename |
| 918 | "cannot add local filter rules in long-named directory" (path composed from rule text) |
| 1101-1102 | match trace ("...ing file X because of pattern Y") |
| 1377-1378 | "invalid modifier%s in filter rule: %s" |
| 1535 | pattern by explicit length |
| 1622 | `MAX_MERGE_DEPTH` exceeded |
| 1659-1660 | "hidden by daemon filter" |
| 1699-1700 | `parse_filter_file` debug trace |
| 1712 | "failed to open %sclude file" |
| 1801 | "discarding over-long filter" |

The debug and trace sites are in scope, not optional extras: the comment at
`:1567` documents a real observed leak through the match trace. Both trace
gates are `DEBUG_GTE(FILTER, n)` - `n = 2` for `add_rule`, and `n = 1` for a
sender or generator / `3` for a receiver on the match trace. They are reached
with `--debug=FILTERn`, not with `-vv`.

## 8. Refusal observable

`filter_rule_err` (`exclude.c:133-137`) prints `"%s: %s"` and calls
`exit_cleanup(RERR_SYNTAX)`, i.e. exit 1:

```c
rprintf(FERROR, "%s: %s\n", msg, rule_text(NULL, rulestr));
```

It has four call sites, and every one of them is redacted by that single
`rule_text`:

| Line | Message |
|---|---|
| 1363 | `Unknown filter rule` |
| 1452 | `specified-side merge file contains specified-side filter` |
| 1470 | `'!' rule has trailing characters` |
| 1475 | `unexpected end of filter rule` |

Any message-wording work on these four is therefore coupled to the provenance
funnel: matching upstream's text without the funnel produces upstream's words
carrying peer-controlled content.

## 9. Confirmed behaviour

Each rule above predicts a specific string. Measured against the 3.5.0 binary,
every probe paired with an argument-sourced control:

| Rule | Probe | Observed |
|---|---|---|
| §1 deferred | per-dir merge named by a file | `<rule from a file read earlier>` |
| §3 arm 2 | `outer.rules` merges `g.rules`; bad rule in `g.rules` | `<rule from a file named at outer.rules line 1>` |
| §3 arm 3 | `.w w.rules` (word-split) | `<rule from w.rules>`, no line number |
| §3 arm 4 | bad rule on line 2 / line 4 | `line 2` / `line 4` |
| §3 physical lines | comment + blank + rule + bad rule | `line 4` |
| §2 `rule_detail` | `-k foo` as argument | `invalid modifier 'k' at position 1 in filter rule: -k foo` |
| §2 `rule_detail` | same rule inside a file | `invalid modifier in filter rule: <rule from m.rules line 1>` |
| §2 rotation | argument-named dir-merge, `--debug=FILTER3` | `add_rule(<rule from /…/.sub-rules line 1>) [per-dir .sub-rules]` |
| §4 exclude-self | file-named dir-merge, `--debug=FILTER3` | `add_rule(<rule from a file read earlier>)` |
| §5 nesting | merge on line 2, bad rule on line 3 | `<rule from f.rules line 3>` |
| §6 errno | missing merge file named by a file | `failed to open exclude file <rule from n.rules line 1>` |
| §6 control | missing file named by an argument | `... missing.rules: No such file or directory (2)` |
| §6 over-long | 200 KB line | `discarding over-long filter: <rule from big.rules line 1>` |

The rotation row is the sharpest: one message holds a redacted fragment and a
verbatim one at once, so an unrotated buffer would visibly print the same
string twice.

Note the modifier offset is 0-based (`-k foo` reports position 1).

## 10. Consequences for oc-rsync

oc-rsync currently echoes peer-controlled merge-file text, and emits three
different messages where upstream emits one:

| Source | oc-rsync site | Divergence |
|---|---|---|
| argument | `crates/cli/src/frontend/filter_rules/parsing/rules.rs:184` | wording: "unsupported filter rule ... this build currently supports only ..." where upstream says "Unknown filter rule" and echoes verbatim |
| merge (`.`) | `crates/cli/src/frontend/filter_rules/merge.rs:279` | wraps the above, and additionally prints a canonicalised **absolute path** to the merge file |
| dir-merge (`:`) | `crates/engine/src/local_copy/dir_merge/parse/line.rs:364` | echoes the literal line; wording otherwise closest to upstream |

A fourth emitter, `crates/filters/src/merge/parse.rs:603`, shares the dir-merge
shape; its reachability is not yet traced.

A fifth is `crates/cli/src/frontend/filter_rules/merge.rs:237` plus its
dir-merge twin in `crates/engine/src/local_copy/dir_merge/load.rs`, which
implement upstream's `exclude.c:1452` refusal:

```
upstream: specified-side merge file contains specified-side filter: <rule from f.rules line 1>
oc:       specified-side merge file contains specified-side filter: -r foo
```

This site is worth calling out as the clearest evidence for section 0's
argument about the chokepoint. It is unconditionally file-sourced - the guard
fires only when a merge template names a side *and* a rule inside the merged
file names one - so there is no argument-sourced case to preserve, and the
`rulestr` is always a line the peer chose. It is also recent, which is the
point: the message was added to oc after the surrounding parser was already
correct, exactly the "a message added later reintroduces the leak" failure
mode the upstream comment describes. Per-message fixes do not prevent this;
only routing every emitter through one funnel does.

### The plumbing splits in two, and only one half is cheap

There are two classes of emitter, and they differ in whether provenance is
still in scope:

- **Parse-time errors** (`filter_rule_err` analogues, merge-open failures). The
  parser knows which file it is reading, so a `RuleSource` can be built at or
  near the call site. `crates/filters/src/merge/parse.rs` already threads
  `source_path` + `line_num`; `crates/cli/src/frontend/filter_rules/merge.rs`
  has the merge file's display name and can count lines. Cheap.

- **Trace-time messages** (`add_rule`, the match trace). By the time a rule is
  compiled, `FilterRule`/`CompiledRule` hold `pattern: String` and nothing
  else - oc has **no field equivalent to `FILTRULE_FROM_FILE`**
  (`rsync.h:1055`). Provenance is already lost. These sites cannot redact
  until a provenance bit is carried on the rule itself, through parsing and
  into the compiled form.

That per-rule bit is section 1's second predicate input, and it is exactly
what upstream needed too - `FILTRULE_FROM_FILE` exists precisely because the
dynamic "am I parsing a file right now" flag is false by the time a deferred
or compiled rule is used. An implementation that adds only the dynamic half
will redact the parse errors and silently leave every trace site leaking.

oc's trace sites, for reference (all reachable via `--debug=FILTER`):
`crates/filters/src/compiled/mod.rs` and
`crates/engine/src/local_copy/filter_program/segments.rs` (add_rule, level 2);
`crates/filters/src/decision.rs` and the same `segments.rs`
(`report_filter_result`, level 1). Note oc runs both traces **twice, in two
crates**, where upstream has one funnel in `exclude.c`.

Three upstream trace sites have no oc analogue at all: the `" [per-dir %s]"`
label (`:379`), "hidden by daemon filter" (`:1659-1660`), and the
`parse_filter_file` trace (`:1699-1700`). oc also gates its match trace at
level 1 unconditionally, with no analogue of upstream's receiver arm
(`DEBUG_GTE(FILTER, am_sender||am_generator ? 1 : 3)`).

Two properties of the upstream design should survive the port:

1. One funnel with a provenance parameter, not a fix per message. The value is
   that a message added later cannot reintroduce the leak.
2. The argument arm stays verbatim. Any test suite for this work needs an
   argument-sourced control on every case, or a blanket-redaction regression
   passes silently.

Sequencing note: the wording-parity work on the four `filter_rule_err`
messages is tracked separately, but it is not independent of this document -
see section 8. Landing upstream's wording without the funnel gives upstream's
text wrapped around peer-controlled content, which is worse than the current
state because it looks correct. Either sequence the funnel first, or land both
together. Both changes alter strings that existing filter tests assert on.

Out of scope: `daemon_config_filter_file` (`exclude.c:1681`,
`clientserver.c:931/954`) gates `operator_path_resolve` so the daemon's own
`filter` / `include from` / `exclude from` parameters may live outside the
module. That is path confinement, covered by
[upstream-3.5.0-path-confinement-model.md](upstream-3.5.0-path-confinement-model.md).

## References

- `exclude.c:47` - `daemon_config_filter_file`
- `exclude.c:49-62` - threat model and the four provenance globals
- `exclude.c:67-69` - `TEXT_FROM_FILE`
- `exclude.c:72-86` - `rule_src_where`
- `exclude.c:88-123` - `rule_text_len` / `rule_text`, the chokepoint
- `exclude.c:127-131` - `rule_detail`
- `exclude.c:133-137` - `filter_rule_err`, `RERR_SYNTAX`
- `exclude.c:1265` - `FILTRULE_FROM_FILE` set during file parsing
- `exclude.c:1567` - exclude-self rule inherits provenance
- `exclude.c:1705-1717` - failed open, errno suppression
- `exclude.c:1734-1760` - save, snapshot, and per-file setup
- `exclude.c:1800-1801` - over-long line, length 0
- `exclude.c:1807` - `rule_src_file` re-set inside the read loop
- `exclude.c:1814-1816` - restore on exit
- `rsync.h:1055` - `FILTRULE_FROM_FILE` (1<<21)

# Validate Stateful Claims (META-VSC)

## Purpose

Persistent state outside the working tree drifts. Memory notes, task-tracker
statuses, architectural-debt summaries, and "SHIPPED" markers describe past
intent — not present reality. Acting on stale state has produced incorrect
closures and false-clean audits. This spec defines when and how to re-verify
such claims against the code before relying on them.

## Scope: what counts as a "stateful claim"

A stateful claim is any persisted assertion about the current state of the
repository that lives outside `git` history on `master`. In practice:

- Memory notes (project / feedback / reference files under the agent memory
  directory).
- Task-tracker statuses ("DONE", "SHIPPED", "RESOLVED", "PARTIAL", "WIP",
  "SHELVED") attached to issue or task IDs.
- Architectural-debt one-liners — entries that summarise an open or closed
  gap (for example `IFX-N`, `RSS-*`, `WPC-*`, `PIP-*`).
- `SHIPPED` markers in summary docs, release notes, or roadmap tables.
- "Already landed", "already wired", "production-ready" assertions in
  conversational context.

Statements grounded in the working tree itself — source files, tests,
`Cargo.toml`, `git log` on `master` — are not stateful claims for the purposes
of this spec. They are the ground truth that stateful claims must be validated
against.

## When validation MUST happen

Validation is mandatory before:

1. Recommending an action whose justification rests on a stateful claim
   ("we can close X because memory says it shipped").
2. Closing or marking a task whose current status was inherited from memory
   rather than from work the current session performed.
3. Citing a stateful claim in a PR title, PR body, commit message, design
   doc, or audit report.
4. Skipping work on the grounds that "it is already done" when the only
   evidence is a memory note or a status marker.
5. Asserting that a gap, regression, or audit finding is resolved.

Validation is also required when a stateful claim contradicts what the code
appears to show. Surface the conflict; pick the code.

## How to validate

The validation procedure is grep-first, evidence-cited, code-anchored:

1. Re-grep the code paths that the claim refers to. Confirm the named
   functions, types, or files exist and behave as described.
2. Re-check the task tree (issue tracker, PR list, label state). Confirm
   the referenced PR landed on `master`, not just that it was opened.
3. Cite `file:line` evidence in any audit, recommendation, or PR body.
   "Memory says X" is not evidence. "`crates/foo/src/bar.rs:142` shows X"
   is evidence.
4. Cross-check against `git log -- <path>` on `master`. If a claim says a
   change shipped, the diff must be reachable from `origin/master`.
5. If the claim references a feature flag, runtime gate, or `cfg`, verify
   the gate state in source and the default build configuration.

Never trust a stateful claim in isolation. A claim plus citable code is
acceptable; a claim alone is not.

## When skipping validation is OK

Re-validation is unnecessary in narrow cases:

- The claim describes work the current session itself just performed and
  the artefacts are still in the working tree or local `git log`.
- The claim is purely ephemeral conversational context for an in-flight
  decision (for example, "the patch I am about to write touches `foo.rs`")
  rather than a persisted status assertion.
- The validation would be strictly more expensive than redoing the work
  the claim describes. In that case prefer redoing the work.

Skipping is never OK when the action being taken on the basis of the claim
is destructive (closing a tracked issue, deleting a design doc, removing
test coverage, marking a debt item resolved in shared docs).

## Reference cases

Two memory notes capture concrete instances that motivated this spec.

**False-status case: `project_rss_arena_not_landed.md`.**
The memory index initially recorded that `RSS-7/8/9` had shipped. A later
audit found the production `FileEntry` had never been migrated; only a
dead `bumpalo` prototype and design docs existed. The "false `PathHandle`
premise" was the consequence of treating a status marker as evidence
instead of re-checking the code. Under this spec, an agent acting on the
`RSS-7/8/9` status would have been required to grep for the production
type and discover the gap before recommending follow-up work.

**Validation-pattern-that-worked case: `project_wpc_verify_audit_clean.md`.**
The `WPC-VERIFY` family (`WPC-3/4/8/9`) was re-audited on 2026-06-12 with
the explicit goal of confirming that ADS and reparse-point handling were
present and wired. The audit checked the code paths, cited the evidence,
and recorded the finding. The note was then marked clean with an explicit
"do not re-audit unless triggers fire" caveat. This is the shape every
stateful-claim validation should take: code-anchored, citation-bearing,
and bounded by a re-trigger condition.

## Procedure summary

For any stateful claim that gates an action:

1. Locate the claim and the action it gates.
2. Identify the code paths the claim refers to.
3. Grep, read, and cite `file:line` evidence on `master`.
4. Cross-check against `git log` and PR state.
5. If evidence agrees, proceed and record the citations alongside the
   action.
6. If evidence disagrees, surface the conflict, update the stateful claim
   to match reality, and only then decide the action.

The default posture is distrust of persisted state and trust of cited code.

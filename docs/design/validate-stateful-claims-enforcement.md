# Validate Stateful Claims: Enforcement Mechanism (META-VSC-2)

## Context

`docs/design/validate-stateful-claims.md` (META-VSC-1) specifies the policy:
re-verify persisted-state assertions (memory notes, task-tracker statuses,
architectural-debt summaries, `SHIPPED` markers) against the working tree
before acting on them. The policy text is settled; this doc picks the
mechanism that ENFORCES it across agent sessions.

## Options

Three enforcement shapes were considered.

**(a) Skill.** A `/superpowers`-style skill (for example
`validating-stateful-claims`) that the loop or iteration directive invokes
at the moment an agent is about to act on a stateful claim. The skill body
walks the agent through the grep-first procedure from META-VSC-1 (locate
claim, grep code, cite `file:line`, cross-check `git log`, decide).

**(b) Memory note.** A `feedback_*.md` entry under the agent memory tree
that loads into every conversation. The note states the standing rule
("distrust persisted state; trust cited code") and references the policy
doc. Discoverable in every session through the existing memory-load
pipeline, the same channel as `feedback_consolidate_cargo.md` and
`feedback_validate_state_first.md`.

**(c) Both.** Skill plus memory note, with a strict separation of concerns:
the memory note carries the WHAT (the standing rule), the skill carries
the WHEN (the active validation step at the point of use).

## Comparison

| Dimension              | (a) Skill only            | (b) Memory note only      | (c) Both                  |
| ---------------------- | ------------------------- | ------------------------- | ------------------------- |
| Discoverability        | Only when invoked         | Loads every conversation  | Loads + invocable on demand |
| Enforcement strength   | Strong at the call site   | Passive reminder; easy to skim past | Strong at call site, reinforced by standing rule |
| Maintenance cost       | One skill file + invocation wiring | One ~30-line memory note  | Both; mitigated by clean separation |
| Failure mode           | Agent never invokes -> silent skip | Agent reads but ignores -> silent skip | Skill missed -> memory still nags; memory missed -> skill still enforces at use |
| Precedent in this repo | No existing validation skill | `feedback_validate_state_first.md` already enforces a sibling rule | Mirrors `consolidate_cargo` (memory rule) + per-task prompt enforcement |

The two single-mechanism options each have a fatal failure mode. A skill
alone goes unused unless the loop directive explicitly fires it, and the
loop directive is itself a stateful artefact subject to drift. A memory
note alone has no teeth at the moment of action; passive reminders demonstrably
lose to action pressure (the RSS-7/8/9 false-status case in META-VSC-1
happened in a repo that already had `feedback_validate_state_first.md`
loaded - reading is not enforcement).

## Recommendation: (c) both

Adopt the dual mechanism with a clean split.

**Memory note carries the WHAT (standing rule).**
A new `feedback_validate_stateful_claims.md` states the rule in one
paragraph, cites `docs/design/validate-stateful-claims.md` as the spec,
and lists the four triggers from META-VSC-1 (PR/commit citation, task
closure, skipping work, "already done" claims). This is the
always-on reminder. It does NOT restate the procedure - it points at
the spec doc and at the skill.

**Skill carries the WHEN (active validation step).**
A new `validating-stateful-claims` skill is invoked at the point an agent
is about to act on a stateful claim. The skill body walks the procedure
(grep, `file:line` cite, `git log` cross-check, conflict surface). The
loop / iteration directive declares the skill as the mandatory pre-action
step when the action depends on a stateful claim. This is the active
checkpoint.

**No duplication.** The memory note does not contain the procedure; the
skill does not restate the standing rule. The spec doc
(`docs/design/validate-stateful-claims.md`) remains the single source of
truth that both reference. If the spec changes, the skill and memory note
are kept thin enough that updates are cheap.

## Why not (a) alone

A skill that no directive invokes is dead weight. The existing
`feedback_*` channel already proves agents read in-context memory notes
even when they ignore skill catalogs they never search. Removing the
memory-note layer would leave first-touch sessions without any signal
that the policy exists.

## Why not (b) alone

The RSS-7/8/9 case happened with `feedback_validate_state_first.md`
already in memory. Standing rules are necessary but not sufficient. A
procedure-bearing skill at the call site converts reading into action.

## Follow-ups

- **META-VSC-3 (pilot on UTS family).** Pilot the skill on the UTS
  workstream, where stateful claims (test-status markers, "RESOLVED" /
  "REOPEN" memory notes) directly gate close/reopen decisions. UTS is
  the highest-friction surface for the policy and the cleanest pilot.
- **META-VSC-5 (memory-note + `/loop` wiring).** Write the
  `feedback_validate_stateful_claims.md` memory note, register the
  `validating-stateful-claims` skill, and wire it into the `/loop`
  iteration directive as a mandatory pre-action step for any task whose
  trigger is a stateful claim.

META-VSC-3 and META-VSC-5 can run in parallel: the pilot proves the
skill body, the wiring delivers the memory note and global hook.

## Scope notes

This doc decides the mechanism. It does NOT author the skill body, write
the memory note, or modify the `/loop` directive. Those are META-VSC-3
and META-VSC-5 deliverables. No code, no skill files, no memory-tree
edits land with this PR.

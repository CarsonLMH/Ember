# Ember documentation

This directory separates the current engineering contract from the decisions
and review history that explain how Ember reached it. That distinction matters:
an old review can be useful evidence without describing the code that runs
today.

## Start here

| Document | Status | Use it for |
|---|---|---|
| [Product specification](../SPEC.md) | **Current contract** | Product behavior, priorities, performance budgets, and non-goals |
| [Agent and maintainer guide](../AGENTS.md) | **Current contract** | Architecture boundaries, invariants, commands, and definition of done |
| [Architecture](ARCHITECTURE.md) | **Current contract** | Runtime components, data flows, concurrency, and ownership boundaries |
| [Privacy and local data](PRIVACY.md) | **Current contract** | Stored data, local processing, deletion semantics, and threat boundaries |
| [Testing](TESTING.md) | **Current contract** | CI, unit tests, hidden native E2E, visible checks, and real-photo gates |
| [Benchmarks](BENCHMARKS.md) | **Measured evidence** | Metric definitions, current reference results, and a reproduction protocol |
| [Metadata contract](METADATA.md) | **Current contract** | XMP/xattr fields, adoption precedence, and file-safety guarantees |
| [Face-system deviations](FACES_DEVIATIONS.md) | **Accepted decisions** | Deliberate differences between the original face plan and the accepted product |
| [Rename record](RENAME.md) | **Accepted migration record** | The completed ApolloTwo-to-Ember data migration and its deliberate legacy identifiers |

The bundled face models and their licence texts live in
[`src-tauri/models/`](../src-tauri/models/). The public screenshot and its
privacy-safe source are documented in [assets](assets/README.md).

## How to read the historical material

The following files are retained as time-bounded evidence. They are **not the
current implementation contract**, and an open finding in an early review may
have been fixed by a later round:

- [Initial face-plan review](FACES_PLAN_REVIEW.md) and
  [revision-two review](FACES_PLAN_REVIEW_REV2.md)
- [Face implementation review archive](reviews/faces/)
- [Claude Code and Codex review-gate proposal](CLAUDE_CODE_CODEX_REVIEW_GATES.md)

For current face behavior, read the specification, `AGENTS.md`,
`FACES_DEVIATIONS.md`, and the implementation/tests together. For current
agent-harness behavior, use `AGENTS.md`, `.mcp.json`, `.codex/config.toml`, and
`npm run test:harness`; the review-gate document describes a proposed workflow,
not proof that every part of it is installed.

## Source-of-truth order

When documents disagree, use this order and surface the conflict:

1. `SPEC.md` for confirmed product behavior and priority.
2. `AGENTS.md` for current engineering invariants and operating rules.
3. A named accepted-decision record, currently `FACES_DEVIATIONS.md`, where it
   explicitly amends an earlier plan.
4. Current source and executable tests for what the checked-out revision does.
5. Historical plans, reviews, and fix reports as evidence of past reasoning.

Source and tests can reveal drift from the intended contract; they do not
silently rewrite it. A contribution that finds such drift should call it out
instead of choosing a side by accident.

## Keeping these docs useful

- Update the relevant current-contract page when behavior, storage, a command,
  or a safety boundary changes.
- Add an accepted-decision note when real use deliberately changes a plan.
- Keep historical reviews intact; add a later disposition instead of editing
  old evidence into apparent foresight.
- Use repository-relative paths and privacy-safe examples. Never add personal
  photo paths, EXIF, face labels, database contents, or real-photo screenshots.

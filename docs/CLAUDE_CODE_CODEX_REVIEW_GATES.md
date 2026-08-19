# Claude Code + Codex review gates

## Objective

Add independent Codex review to Claude Code at two high-leverage boundaries:

1. Review non-trivial implementation plans before Claude leaves Plan Mode.
2. Review completed, tested code slices before Claude commits them.

The system should improve correctness without invoking Codex after every edit or turning development into an indefinite multi-agent debate. Claude remains the author, Codex is an independent reviewer, and the user remains the final decision-maker.

## Desired workflow

```text
High-risk feature request
        |
        v
Claude writes a Plan Mode plan
        |
        v
ExitPlanMode hook invokes Codex read-only
        |
        +-- APPROVE ----------> normal human plan approval
        |
        +-- CHANGES_REQUIRED -> findings returned to Claude
                                  |
                                  v
                             Claude revises plan
                                  |
                                  v
                            one follow-up review
                                  |
                                  v
                         user arbitrates if unresolved

Implementation proceeds in bounded slices
        |
        v
Claude runs tests and gates
        |
        v
/codex-code-review reviews the stable diff
        |
        +-- APPROVE ----------> commit allowed
        |
        +-- CHANGES_REQUIRED -> Claude fixes valid findings,
                                reruns relevant tests and review
```

## Review policy

Do not require the same ceremony for every change.

| Change type | Plan review | Code review |
|---|---:|---:|
| Documentation, copy, CSS, trivial test | No | Usually no |
| Localized bug fix following an established pattern | No | Optional final diff |
| Normal feature using established architecture | Optional | Once before commit |
| Migration, persistence, concurrency, or hot-path work | Yes | Each risky slice |
| Security, privacy, data loss, or new native runtime | Yes | Risky slices plus final branch |

For Ember, require plan review when work includes any of:

- SQLite schema or data migration;
- background workers, synchronization, or concurrency;
- verdict durability or filesystem mutation;
- flip-loop or cold-open performance;
- sensitive local data or privacy behavior;
- native dependencies, application packaging, or model resources;
- cross-platform behavior;
- changes spanning several architectural layers.

## Proposed project structure

```text
.claude/
  hooks/
    codex-plan-review.sh
    require-code-review.sh
  scripts/
    codex-code-review.sh
    review-common.sh
  skills/
    codex-code-review/
      SKILL.md
    codex-plan-review/
      SKILL.md                 # optional manual fallback
  review-schemas/
    plan-review.schema.json
  reviews/                     # gitignored receipts and raw output
  settings.json
docs/
  plan-reviews/                # optional tracked architectural reviews
```

Add `.claude/reviews/` to `.gitignore`. Track only reviews that record meaningful architectural decisions, copying those deliberately to `docs/plan-reviews/`.

## Phase 1: Plan review gate

### Hook configuration

Add a `PreToolUse` hook for `ExitPlanMode` to `.claude/settings.json`:

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "ExitPlanMode",
        "hooks": [
          {
            "type": "command",
            "command": "${CLAUDE_PROJECT_DIR}/.claude/hooks/codex-plan-review.sh",
            "args": [],
            "timeout": 600
          }
        ]
      }
    ]
  }
}
```

Claude Code injects the active plan into the hook input:

```json
{
  "tool_name": "ExitPlanMode",
  "tool_input": {
    "plan": "## Feature plan...",
    "planFilePath": "/Users/.../.claude/plans/feature.md"
  }
}
```

### Hook behavior

`codex-plan-review.sh` should:

1. Read all JSON hook input from stdin.
2. Extract `.tool_input.plan` and `.tool_input.planFilePath` with `jq`.
3. Calculate a receipt key from:
   - plan content;
   - current Git `HEAD`;
   - current dirty-worktree fingerprint;
   - review prompt/schema version.
4. Reuse a cached approved receipt for an identical key.
5. Invoke Codex from the repository root in a read-only, non-interactive, ephemeral run.
6. Save the structured result under `.claude/reviews/plan-<hash>.json`.
7. If the verdict is `APPROVE`, exit 0 with no hook decision. Claude Code then presents its normal human Plan Mode approval prompt.
8. If the verdict is `CHANGES_REQUIRED`, return a `PreToolUse` denial whose reason contains the concise findings and review artifact path. Claude remains in Plan Mode and receives the findings.
9. On execution, authentication, malformed-output, or timeout failure, fail closed with an actionable message and documented manual bypass.

Do not return an automatic `allow` for `ExitPlanMode`; Codex approval must not replace human approval.

### Codex invocation

Use `codex exec`, not `codex review`, because this is an architectural plan rather than a code diff.

Conceptual invocation:

```bash
codex exec \
  -C "$CLAUDE_PROJECT_DIR" \
  --ephemeral \
  --sandbox read-only \
  --ask-for-approval never \
  --output-schema "$CLAUDE_PROJECT_DIR/.claude/review-schemas/plan-review.schema.json" \
  -o "$RESULT_FILE" \
  -
```

Generate the prompt on stdin without interpolating untrusted plan text into a shell command. Treat the plan as data, not shell syntax.

Codex should inspect the repository but must not edit it. Prefer deterministic local behavior; do not enable network access or additional MCP servers for the review unless a particular plan genuinely requires current external documentation.

### Plan review schema

```json
{
  "type": "object",
  "properties": {
    "verdict": {
      "type": "string",
      "enum": ["APPROVE", "CHANGES_REQUIRED"]
    },
    "summary": { "type": "string" },
    "blocking_findings": {
      "type": "array",
      "items": {
        "type": "object",
        "properties": {
          "id": { "type": "string" },
          "severity": {
            "type": "string",
            "enum": ["critical", "high", "medium"]
          },
          "title": { "type": "string" },
          "evidence": { "type": "string" },
          "recommendation": { "type": "string" }
        },
        "required": [
          "id",
          "severity",
          "title",
          "evidence",
          "recommendation"
        ],
        "additionalProperties": false
      }
    },
    "nonblocking_findings": {
      "type": "array",
      "items": { "type": "string" }
    }
  },
  "required": [
    "verdict",
    "summary",
    "blocking_findings",
    "nonblocking_findings"
  ],
  "additionalProperties": false
}
```

### Plan reviewer prompt

Use a neutral, evidence-driven prompt. Do not include Claude's hidden reasoning or tell Codex which verdict is desired.

```text
Act as an independent architecture reviewer.

Read the supplied implementation plan and inspect the existing repository deeply
enough to validate its architectural claims. Do not edit files.

Prioritize:
1. correctness and data-loss risks;
2. violations of established project invariants;
3. concurrency, lifecycle, migration, and recovery behavior;
4. performance on user-facing hot paths;
5. security and privacy;
6. missing verification and acceptance tests;
7. unnecessary complexity that materially increases implementation risk.

A blocking finding must cite concrete plan text and repository evidence. Do not
block for naming, formatting, style preferences, or speculative enhancements.

Return APPROVE only when implementation can safely begin. Otherwise return
CHANGES_REQUIRED with bounded, actionable findings.
```

### Plan-review stopping rule

- Initial Codex review.
- One follow-up after Claude revises the plan.
- If substantive disagreement remains, Claude stops and asks the user to arbitrate.
- Never let Claude and Codex negotiate indefinitely.

Record the iteration count in the receipt or derive it from reviews sharing the same plan family/session.

## Phase 2: Strategic code review

### Manual/project skill

Create `.claude/skills/codex-code-review/SKILL.md`:

```markdown
---
name: codex-code-review
description: Run an independent Codex review of a completed and tested implementation slice before committing. Use only when a coherent slice is ready, not after individual edits.
disable-model-invocation: false
allowed-tools: Bash(${CLAUDE_PROJECT_DIR}/.claude/scripts/codex-code-review.sh *)
---

Review the completed implementation slice described by `$ARGUMENTS`.

1. Confirm the relevant tests and project gates have been run successfully.
2. Run `${CLAUDE_PROJECT_DIR}/.claude/scripts/codex-code-review.sh "$ARGUMENTS"`.
3. Read the resulting report.
4. Address valid blocking findings only; do not implement stylistic suggestions.
5. Rerun affected tests after fixes.
6. Rerun the review once if code changed materially.
7. If disagreement remains, ask the user to arbitrate.
```

The exact `allowed-tools` syntax should be validated against the installed Claude Code version during implementation.

### Review script

`codex-code-review.sh` should:

1. Refuse to run on an empty diff.
2. Capture `HEAD`, a complete diff fingerprint, and optional slice description.
3. Optionally require a recent successful gate receipt, or clearly warn when tests have not been recorded.
4. Reuse an existing review receipt for an identical diff.
5. Run:

```bash
codex review --uncommitted
```

6. Save the raw review and a small receipt under `.claude/reviews/`.
7. Print the review path and outcome for Claude.

Codex also supports:

```bash
codex review --commit HEAD
codex review --base main
```

Use `--uncommitted` for a completed slice before committing, `--commit HEAD` when reviewing an already-created milestone, and `--base main` once at the end of a large multi-slice feature.

Do not invoke Codex after every file edit. Review a stable diff only.

### Code-review rubric

Put durable project-specific review priorities in repository guidance loaded by Codex, or use the supported custom-instruction route after verifying how the installed CLI combines custom instructions with review targets.

The rubric should be:

```text
Review this completed, tested slice.

Prioritize:
1. data loss or incorrect persisted state;
2. concurrency and lifecycle races;
3. violations of documented project invariants;
4. user-facing performance regressions;
5. failure and crash recovery;
6. security and privacy;
7. missing tests that would expose a plausible defect.

Do not report naming, formatting, stylistic preferences, or speculative
refactors unless they conceal a correctness problem. Cite file and line evidence
for every blocking finding.
```

### Code-review receipt

```json
{
  "kind": "code-review",
  "version": 1,
  "gitHead": "<sha>",
  "diffHash": "<sha256>",
  "reviewedAt": "<ISO-8601>",
  "reviewer": "codex",
  "verdict": "APPROVE",
  "blockingFindings": [],
  "reportPath": ".claude/reviews/code-<hash>.md"
}
```

Any material code edit changes the diff hash and invalidates the receipt. Documentation-only changes may be excluded from the fingerprint only if the implementation does so explicitly and safely.

## Phase 3: Commit safety net

Add a Claude Code `PreToolUse` hook for Bash and, on Windows, PowerShell. The hook should only activate for a `git commit` command.

Its job is not to launch a slow review at commit time. It should only verify that:

- the current diff is non-empty;
- the current diff hash has an approved review receipt when policy requires one;
- the receipt was produced against the current `HEAD`;
- the receipt has no blocking findings.

If no fresh receipt exists, deny the commit and tell Claude to run `/codex-code-review`.

Whether a review is required can be controlled by one of:

- a session/slice marker written when a high-risk plan is approved;
- a project policy requiring review for every non-trivial commit;
- an explicit `REVIEW_REQUIRED` marker in a slice file;
- a user-controlled bypass for trivial or emergency changes.

Avoid brittle shell-command parsing. Match the hook narrowly using Claude Code's tool matcher/`if` facilities, then validate the parsed command defensively in the script.

Do not install this as a repository Git pre-commit hook initially; a Claude-specific hook avoids unexpectedly changing the user's normal terminal Git workflow.

## Authentication and sandboxing

- Local `codex exec` and `codex review` should reuse the user's saved Codex CLI authentication.
- Never place `OPENAI_API_KEY`, `CODEX_API_KEY`, or Codex authentication files in the repository, Claude settings, prompts, or review artifacts.
- Treat `~/.codex/auth.json` as a credential.
- Plan review must be read-only and non-interactive.
- Review scripts must quote paths and treat plan/diff text as data.
- Do not use `danger-full-access`.
- For future CI automation, prefer the official Codex GitHub Action and tightly scoped secrets rather than exporting an API key job-wide.

## Failure and bypass behavior

The workflow must not trap the user permanently.

Recommended behavior:

- Codex returns findings: follow the normal review loop.
- Codex is unavailable or unauthenticated: fail closed and show the exact recovery command.
- Structured output is malformed: fail closed, preserve raw output, and show its path.
- Review times out: fail closed and allow the user to retry.
- User intentionally overrides: require an explicit environment variable or local override file, record the bypass in the hook output, and never enable it silently.

Example local-only bypass:

```text
CODEX_REVIEW_BYPASS=1
```

Do not commit bypass state.

## Pacing rules

Never invoke Codex:

- after every `Edit`, `Write`, or patch;
- on every Claude `Stop` event;
- continuously while a diff is changing;
- before relevant tests pass;
- repeatedly for an unchanged plan or diff;
- for ordinary formatting, copy, or documentation-only changes.

Invoke Codex:

- once when a high-risk plan is ready to leave Plan Mode;
- once more after substantive plan corrections;
- once after a coherent, tested, high-risk implementation slice;
- once at the end of a large multi-slice feature against its base branch.

Do not run plan or code review asynchronously while Claude continues editing the reviewed artifact; that produces stale findings.

## Ember-specific cadence

For a feature comparable to Faces:

1. Plan review: initial plus at most one revision.
2. Dependency/performance spike: review benchmark validity and packaging conclusions.
3. Persistence/worker foundation: mandatory code review.
4. Automatic matching or other sensitive algorithm: mandatory code review.
5. Straightforward UI/filter integration: optional review unless the diff becomes broad.
6. Rescan/migration/recovery work: mandatory code review.
7. Final feature branch: one `codex review --base main` integration review.

Do not review every small subcommit independently.

## Verification of the review system

### Plan hook tests

- Approved fixture allows the normal Plan Mode approval flow.
- `CHANGES_REQUIRED` fixture denies `ExitPlanMode` and sends findings to Claude.
- Identical plan/HEAD/diff reuses the cached receipt.
- Plan edit invalidates the cached receipt.
- Repository edit or `HEAD` change invalidates the receipt.
- Timeout, missing `codex`, missing `jq`, authentication failure, and malformed JSON fail closed with actionable messages.
- Manual bypass is explicit, local, and visible.
- Plan content containing quotes, command substitutions, and newlines is never executed by the shell.

### Code review tests

- Empty diff exits without creating an approval receipt.
- Approved current diff permits Claude's commit command.
- Any code edit invalidates the receipt.
- A new commit changing `HEAD` invalidates the receipt.
- Blocking findings deny commit.
- Review failure cannot accidentally produce approval.
- Untracked files are included in the review fingerprint.
- Review artifacts never contain credentials.
- Normal non-commit Bash commands are unaffected.

### End-to-end acceptance

1. Start Claude Code in Plan Mode with a deliberately flawed test plan.
2. Attempt to exit Plan Mode.
3. Verify Codex review blocks the exit and Claude receives useful evidence.
4. Revise the plan and verify a passing review still requires human approval.
5. Implement a small test slice, run its tests, and invoke `/codex-code-review`.
6. Verify a stale or missing receipt blocks Claude's commit command.
7. Verify an approved, matching receipt permits the commit.

## Rollout order

1. Implement the manual `/codex-plan-review` skill and validate Codex invocation/authentication.
2. Add structured output, hashing, receipts, and fixture tests.
3. Attach the proven script to the `ExitPlanMode` hook.
4. Implement `/codex-code-review` without commit enforcement.
5. Use both workflows manually for several real Ember slices and measure value.
6. Add the commit safety hook only after receipt behavior is trustworthy.
7. Reassess the risk policy after 5-10 reviews.

## Success metrics

Track:

- review wall time;
- blocking findings raised;
- blocking findings accepted by Claude/user;
- false-positive or stylistic findings rejected;
- defects or rework discovered after review;
- duplicate reviews avoided by caching;
- implementation delays caused by the gate.

If reviews mostly produce rejected style suggestions, narrow the rubric or reduce frequency. If they repeatedly catch migration, race, durability, or recovery defects, preserve the gate for those categories.

## Source references

- Codex non-interactive mode, read-only sandbox, structured output, and automation: <https://learn.chatgpt.com/docs/non-interactive-mode>
- Codex CLI reference, including `codex review --uncommitted`, `--commit`, and `--base`: <https://learn.chatgpt.com/docs/developer-commands?surface=cli>
- Claude Code hooks and `ExitPlanMode` hook input/decisions: <https://code.claude.com/docs/en/hooks>
- Claude Code project skills and tool permissions: <https://code.claude.com/docs/en/skills>


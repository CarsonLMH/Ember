---
name: design-audit
description: Audit one Ember UI surface against the AGENTS.md design brief and installed guideline skills, using synthetic or clearly licensed public fixtures by default plus DOM measurements and screenshots. Use when asked to audit or review an Ember surface. Real-photo inspection is an explicit, one-run, local-only exception. Produces an evidence report and slice plan; never implements changes or runs performance gates.
---

# Design audit

Audit one surface per run. The product owner should get root causes first,
findings tied to measurements or crops, an ordered slice plan, what was
deliberately left alone, and a Method section that says exactly what ran.

Files in this skill:

- `surfaces.md` — how to open each surface, its DOM anchor, useful states, and
  interactions that mutate data.
- `measure.md` — JavaScript snippets for geometry, contrast, hit targets,
  focusable controls, overlays, and click-sequence probes.
- `report-template.html` and `scripts/build_report.py` — report skeleton and
  image inliner.
- `scripts/em.sh` — wrapper over the project-local Tauri MCP CLI, for sessions
  where the MCP tools were not discovered at startup.

## Ground rules (non-negotiable)

1. **The brief outranks every skill.** Read the Design language section of
   `AGENTS.md` first, plus any `docs/*DEVIATIONS*.md` for the surface. If a
   generic skill contradicts Ember's brief, it is not a finding. Record it
   under Deliberately left alone with the conflicting rule cited.
2. **Synthetic or clearly licensed public fixtures are the default.** Start
   with a stable Storybook story when one exists, then use the generated
   synthetic JPEG fixture for integrated Tauri behavior. Do not inspect the
   user's photo library, Ember database, keymap, face chips, cache, or app-data
   directory during ordinary setup. A missing synthetic state is a code-review
   limit, not permission to go looking through private data.
3. **Private data requires explicit, one-run consent.** Before reading any real
   photo, local metadata, face data, or production Ember state, ask the user to
   authorize that exact audit run and scope. A path supplied earlier, consent
   from another run, or the existence of a local database is not consent.
   Consent expires when this audit run ends. If consent is absent or ambiguous,
   stay with synthetic evidence and mark the live state unverified.
4. **Real-photo evidence stays local and is never published.** Do not attach it
   to chat, put it in the repository, publish it with the Artifact tool, place
   it in an issue or pull request, or hand it to another agent or external
   service. Keep screenshots, logs, and the optional local report inside the
   audit scratch directory. Do not save private evidence or identifiers to
   memory.
5. **Redact before writing or capturing.** Redact personal paths, EXIF values,
   face or person labels, and database contents from commands echoed to the
   transcript, screenshots, crops, reports, Method notes, and chat summaries.
   Use placeholders such as `<private-photo-folder>`, `<private-photo>`, and
   `Person A`. In a real-photo webview, replace identifying DOM text only for
   the screenshot or crop, without saving the replacement to Ember. Never copy
   database rows into a report; describe only the state needed for the finding.
6. **Every number comes from the DOM**, never from reading pixels. Use computed
   styles, bounding rectangles, and element counts. Screenshots are evidence
   for the reader; measurements support the claim.
7. **Tag evidence.** Mark each finding *verified live*, *measured*, or *from
   code*. Recheck surprising behavior with a safe live interaction before
   reporting it. If the needed state is unavailable without mutation, report
   it from code and say so in Method.
8. **Run the skills you cite.** If the report names a guideline skill, invoke
   that skill in this run. If you apply an idea from memory, say “from memory”
   rather than naming the skill as executed.
9. **Surface conflicts; do not silently settle them.** When guidance conflicts
   with Ember's conventions, show the options, recommend one, and leave the
   product decision to the owner.
10. **Never audit during a gate run.** The MCP build injects a script into the
    webview and opens a local bridge. It is not the performance build. Stop only
    the process you started, using its exact PID; never pattern-kill.

## Procedure

### 1. Prepare

- Read `AGENTS.md` Design language, the surface entry in `surfaces.md`, the
  component source, its CSS, and any relevant deviations record.
- Create local scratch space outside the repository:

  ```sh
  AUDIT_DIR="$(mktemp -d /tmp/ember-design-audit.XXXXXX)"
  export AUDIT_DIR
  mkdir -p "$AUDIT_DIR/ev"
  ```

- Decide and record the evidence mode: `synthetic`, `public`, or
  `private-with-consent`. Do not start in private mode.

### 2. Create the default synthetic fixture

Prefer a stable Storybook story for React chrome. For the integrated canvas and
native shell, generate disposable JPEGs in the scratch directory:

```sh
node scripts/e2e-fixtures.mjs "$AUDIT_DIR/fixture" 12
EMBER_OPEN="$AUDIT_DIR/fixture" npm run tauri:mcp -- \
  --config '{"identifier":"com.cleung.ember.design-audit"}' \
  > "$AUDIT_DIR/dev.log" 2>&1 &
AUDIT_PID=$!
.claude/skills/design-audit/scripts/em.sh wait "$AUDIT_DIR/dev.log"
.claude/skills/design-audit/scripts/em.sh start
```

The separate identifier isolates the audit from Ember's production database,
settings, and cache. The native folder dialog cannot be driven; `EMBER_OPEN`
opens the generated fixture. If the current session already exposes the Tauri
or Storybook MCP tools, use them instead of `em.sh`.

If a clearly licensed public fixture is necessary, record its source and
license before use, copy only the minimum needed into scratch, and keep the
original read-only. Never use a photo merely because a search result exposes
it. Storybook is useful evidence for stable chrome, but it is not acceptance
evidence for the canvas path or native behavior.

### 3. Optional real-photo inspection

Skip this step unless synthetic, public, and code evidence cannot answer a
material question. Ask a direct question before any lookup, for example:

> For this design-audit run only, may I inspect the real photos and Ember state
> needed for `<surface>` locally? I will not publish the media or expose paths,
> EXIF, face/person labels, or database contents.

Proceed only after an affirmative answer in the current run. Keep the scope to
the named surface and minimum folder or photos needed. Prefer a folder the user
explicitly selects. If selecting suitable state requires the Ember database,
say so in the consent question, open it read-only, suppress row output, and do
not retain query results beyond the run. Never enumerate the library first and
ask permission afterwards.

Use the isolated audit identifier when existing Ember state is unnecessary. If
the surface specifically depends on production ratings, filters, or face state,
explain that production state is required and obtain consent for that too.
Opening production state has benign side effects such as resume-position and
`updated_at` changes; record that fact without recording values or identifiers.

Before each screenshot or crop, temporarily replace identifying DOM strings
with placeholders or exclude those regions. Do not alter the underlying photo,
sidecar, database, person record, or settings. Keep raw screenshots and logs
local even after their visible text is redacted.

### 4. Walk the states

For each safe state in the surface registry:

```sh
em.sh key <key> [Shift]                  # open or toggle
em.sh shot <nn>-<state>                  # full window in $AUDIT_DIR
em.sh js "<snippet from measure.md>"     # DOM measurement
em.sh crop <nn>-<state> <x> <y> <w> <h> <name>
```

Capture the surface at rest, every section at relevant scroll depths, each
menu or popover, and each input. Hover is not capturable, so read the CSS. Run
the geometry, contrast, hit-target, repeated-control, and focusable censuses in
`measure.md`; they are cheap and often reveal the structural problem.

Do not invoke unsafe controls on real or licensed public fixtures. On generated
scratch fixtures, use a mutation only when it is necessary to produce the state
under review, then say so in Method. Nothing in an audit justifies touching a
real verdict, sidecar, trash item, person label, recipe, filter, or setting.

### 5. Code pass with the lenses

Use these lenses in order:

1. Ember's design brief — decides what counts.
2. `macos-design-guidelines` — panels and popovers, control sizes, context
   menus, hover, keyboard vocabulary, undo, transparency, and contrast.
3. `web-design-guidelines` — semantics, labels, live regions, focus
   replacement, nested controls, list sizes, and typography.
4. `apple-design` — press feedback, anchoring, legibility, mapping, agency, and
   wayfinding. Its motion guidance is overruled by Ember's zero-animation flip
   path.
5. `frontend-ui-engineering` — accessible component and copy decisions.

Invoke every skill named in Method. Apply each lens to source with `file:line`
references and promote it to a finding only when the evidence supports it.

### 6. Verify surprising findings live

Claims about ordering, coverage, glitches, or unreachable states need a runtime
probe before they become findings. `measure.md` includes the focusable census,
`elementFromPoint` overlay check, and click-sequence probe. Read state in a
separate call after dispatching an event so React has committed. Known bridge
limits: synthetic Tab does not prove native focus traversal; native dialogs and
drag-and-drop cannot be driven. Mark those limits plainly.

### 7. Write and privacy-check the report

Copy `report-template.html` to `$AUDIT_DIR/<surface>-audit.template.html`, fill
it, and run `scripts/build_report.py` to inline approved crops.

Structure it as: three-sentence verdict, scoreboard, 2–4 root causes, findings
by sub-surface, proposed independently driveable slices, CSS-only quick wins,
deliberately left alone, decisions needed, then Method and limits. Each finding
gets an id, severity, one-line title, evidence with measurements, and a fix size
(`CSS`, `component`, or `redesign`).

Severity: **critical** means unreadable or clipped/unreachable controls;
**major** means a workflow is harder, slower, mouse-only, or accidentally
irreversible; **minor** is polish, consistency, or semantics; **bug** is
functional rather than design, so list it without scoring it.

Before delivery, inspect the final HTML and every crop. Search for workstation
paths and sensitive metadata labels, then visually check that no filename,
EXIF value, person label, database value, or unintended private region remains.
The absence of a text match is not proof that a screenshot is safe.

- For `synthetic` or attributed `public` mode, publish with the Artifact tool
  only after that preflight. Title it `<Surface> Audit`; use the 🪞 favicon and
  republish accepted revisions to the same artifact.
- For `private-with-consent` mode, keep the report in local scratch and open it
  locally for the user. Do not publish it with the Artifact tool, attach it to
  chat, or copy it into the repository—even when the visible text was redacted.

### 8. Close out

- Stop the exact dev PID recorded at launch (`kill "$AUDIT_PID"`; then `wait`
  for it). Restore any real-data UI toggles you changed to their original state.
- For a synthetic or public audit, a memory note may contain the artifact URL,
  root causes, slice order, and open decisions. Never put a local fixture path
  in memory.
- For a real-photo audit, save no memory note and retain no database results.
  Leave the private report and evidence local only; delete them only if the user
  asks.

## Output contract

The deliverable is the public artifact URL for synthetic/public mode or a
locally opened, redacted report for private mode. The self-contained chat
summary gives the evidence mode, root causes in one line each, count by
severity, decisions needed, and every check not run with a reason. For private
mode it contains no screenshot, attachment, path, filename, EXIF value, face or
person label, database detail, or local-report link.

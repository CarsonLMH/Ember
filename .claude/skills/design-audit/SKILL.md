---
name: design-audit
description: Audit the design of one Ember surface (People panel, filmstrip, metadata panel, trash panel, switchers, HUD, face badges…) against the AGENTS.md design brief and the installed guideline skills, using the live dev build through the MCP bridge for screenshots and DOM measurements. Use when asked to "audit the design of X", "design review X", "why does X feel off", or via /design-audit <surface>. Produces a published findings report with evidence crops and a slice plan. Read-only on user data.
---

# Design audit

One surface per run. The output is a report the product owner reads: root
causes first, then findings each tied to a measurement or a crop, then a slice
plan, then what was deliberately left alone, then a Method section that says
exactly what ran. The faces audit (artifact `27ae66f5…`, 2026-08-23) is the
reference run.

Files in this skill:

- `surfaces.md` — registry: how to open each surface, its DOM anchor, the
  states worth capturing, and which interactions mutate user data (never do
  those).
- `measure.md` — JS snippets for the numbers: geometry, contrast, hit targets,
  focusable census, overlay checks, click-sequence probes.
- `report-template.html` + `scripts/build_report.py` — report skeleton and the
  image inliner.
- `scripts/em.sh` — wrapper over the `tauri-mcp` CLI (dev dependency), so the
  loop works even before a session restart has loaded the MCP tools.

## Ground rules (non-negotiable)

1. **The brief outranks every skill.** Read the "Design language" section of
   AGENTS.md first, and `docs/*DEVIATIONS*.md` for the surface if one exists.
   A skill rule that contradicts the brief is not a finding; it goes in the
   report's "Deliberately left alone" list with the rule cited.
2. **Read-only on user data.** The audit opens a real folder from the user's
   real DB. `surfaces.md` lists the unsafe keys and clicks per surface (ratings,
   trash, sort, filters, tag picks, any naming/correction). If a state can only
   be reached by mutating data, review it from code and say so in Method.
3. **Every number comes from the DOM**, never from reading pixels. Computed
   styles, bounding rects, element counts. Screenshots are for the reader;
   measurements are for the claims.
4. **Tag evidence.** Each finding says *verified live*, *measured*, or *from
   code*. Anything surprising (a glitch, an ordering, a coverage claim) gets
   re-verified live with a real interaction before it is written down.
5. **Run the skills you cite.** If the report names `web-design-guidelines`,
   `macos-design-guidelines`, `apple-design` or `frontend-design`, they were
   invoked with the Skill tool in this run. Applying their ideas from memory is
   fine — but then the Method section says "from memory", not the skill name.
6. **Surface conflicts, don't resolve them.** When skills disagree with each
   other or with the app's existing convention (capitalization was the first
   case), the report presents the options and recommends one; the owner
   decides.
7. **Never during a gate run.** The `mcp` build injects a script into the
   webview and runs a WebSocket server. It is not the build the perf budget is
   measured on. Stop only processes you started (exact PID from the dev log),
   never pattern-kill.
8. **Never invent a path.** Folder and photo choices come from a read-only DB
   query (see step 2).

## Procedure

### 1. Prepare

- Read AGENTS.md "Design language"; read the surface's entry in `surfaces.md`;
  read the surface's component(s) and its CSS block in `src/App.css`.
- Create a working dir in the session scratchpad and export it:
  `export AUDIT_DIR=<scratchpad>/audit-<surface>`; `mkdir -p "$AUDIT_DIR/ev"`.

### 2. Pick real data (read-only)

```sh
DB=~/Library/Application\ Support/com.cleung.ember/ember.sqlite3
sqlite3 -readonly -header -column "$DB" "select fo.id, fo.path, count(p.id) photos
  from folders fo join photos p on p.folder_id=fo.id group by fo.id order by photos desc limit 8"
```

Pick the richest folder that exists on disk (`[ -d "$path" ]`). For a surface
with per-photo state (faces, tags, ratings, AF), query for the photo that shows
the most states at once — `surfaces.md` has the query for each.

### 3. Launch with the bridge

```sh
EMBER_OPEN="<folder>" npm run tauri:mcp > "$AUDIT_DIR/dev.log" 2>&1   # run_in_background
.claude/skills/design-audit/scripts/em.sh wait "$AUDIT_DIR/dev.log"    # blocks until the bridge listens
.claude/skills/design-audit/scripts/em.sh start                         # CLI driver session
```

The native folder dialog cannot be driven; `EMBER_OPEN` is the only way to
open a folder. Emitting `tauri://drag-drop` from the bridge does not reach the
webview-scoped listener (tried). If the MCP tools are loaded in this session,
`webview_screenshot` / `webview_execute_js` can replace `em.sh`; the helper
exists so the audit never waits on a restart.

### 4. Walk the states

For each state in the surface's registry entry:

```sh
em.sh key <key> [Shift]            # open / toggle
em.sh shot <nn>-<state>            # $AUDIT_DIR/<nn>-<state>.png, full window
em.sh js "<snippet from measure.md>"
em.sh crop <nn>-<state> <x> <y> <w> <h> <name>   # -> $AUDIT_DIR/ev/<name>.jpg, ≤ 560px wide
```

Always capture: the surface at rest; every section at every scroll depth
(`em.sh js` with `scrollTop`); each menu/popover open; each input visible;
hover is not capturable (read the CSS). Run the geometry, contrast,
hit-target, repeated-control and focusable censuses from `measure.md` on every
surface — they are cheap and they are where the non-obvious findings live.

### 5. Code pass with the lenses, in this order

1. The brief (already read) — decides what counts.
2. `macos-design-guidelines` — panels vs popovers, control sizes (22–28pt),
   context menus, hover, keyboard vocabulary (Esc, Return, Delete, Shift-click),
   undo, reduce-transparency/contrast.
3. `web-design-guidelines` — fetch-and-check list: semantics (`<button>` not
   `<img onClick>`), labels, `aria-live`, focus replacement, nested
   interactives, list sizes, typography (curly quotes, `…`, tabular nums).
4. `apple-design` — non-motion sections only (§1 press feedback, §7 anchoring,
   §12 materials/legibility, §16 mapping, agency, wayfinding). Its spring and
   gesture guidance is overruled by the zero-animation brief.
5. `frontend-design` — copy rules only: one verb per action through button →
   confirm → toast → undo; empty states direct; labels don't do double duty.

Invoke each with the Skill tool. Apply them to the component source with
`file:line` references; promote to a finding only with evidence.

### 6. Verify the surprising ones live

Anything that asserts an order, a coverage, a glitch, or "nothing can reach X"
gets a runtime probe before it is a finding: `measure.md` has the focusable
census, `elementFromPoint` overlay check, and the step-through click probe
(real `em.sh click`/`dblclick`, or `dispatchEvent` with a state read in a
*separate* call — React commits after the handler returns). Known bridge
limits: synthetic Tab does not drive native focus traversal (focus rings stay
unverified — say so); native dialogs and drag-drop cannot be driven.

### 7. Write the report

Copy `report-template.html` to `$AUDIT_DIR/<surface>-audit.template.html`,
fill it, run `scripts/build_report.py` to inline the crops, publish with the
Artifact tool. Title: "<Surface> Audit" (a name, no explainer). Favicon: 🪞 for
every design audit, so they sit together in the gallery. One artifact per
surface; republish to the same path (same URL) as findings close.

Structure, in order: verdict (3 sentences) → scoreboard → root causes (the
2–4 structural things most findings trace to) → findings by sub-surface,
each with id, severity chip, one-line title, evidence paragraph with the
numbers, fix with size (CSS / component / redesign) → proposed slices, each
driveable alone → CSS-only quick wins → deliberately left alone → decisions
needed → method and limits.

Severity: **critical** = unreadable or clipped/unreachable controls; **major**
= a user-facing workflow is harder, slower, mouse-only, or irreversible because
of it; **minor** = polish, consistency, semantics; **bug** = functional, not
design — list it, don't score it.

### 8. Close out

- Stop the dev build you started (PID from `$AUDIT_DIR/dev.log`'s `Running`
  line, or `pgrep -f 'target/debug/ember$'` when it is the only instance).
- Save a memory note: artifact URL, root causes, slice order, open decisions,
  and the folder/photo used so the redesign slices can be driven on the same
  data.

## Output contract

The report is the deliverable. The chat summary is: the URL, the root causes
in one line each, the count by severity, any decision the owner must make,
and every check that was *not* run and why.

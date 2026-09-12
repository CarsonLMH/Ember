# Culling controls and instant filmstrip

## Task 1: Make rating-in-place the default

**Description:** Keep the existing `V` auto-advance preference, but require an
explicit stored opt-in. Preserve the journal-before-UI acknowledgement order and
make automated harnesses move explicitly instead of relying on the preference.

**Acceptance criteria:**

- [x] A fresh profile rates without changing the selected photo.
- [x] An explicit `V` opt-in persists and advances only after a successful
      durable acknowledgement.
- [x] Rating failures change neither verdict nor cursor; filtering a newly
      out-of-scope photo remains honest and documented.

**Verification:**

- [x] Focused session tests cover pending, successful, failed, opt-in, clear,
      and filter-removal behavior.
- [x] Culling E2E asserts position stays fixed after rating on All.
- [x] Chaos/performance harnesses advance explicitly and remain preference-neutral.

**Dependencies:** None

**Files likely touched:** `src/lib/session.ts`, `src/lib/session.test.ts`,
`src/lib/devharness.ts`, `e2e/specs/02-culling.e2e.ts`, `SPEC.md`,
`CHANGELOG.md`

**Estimated scope:** Medium

## Task 2: Add N-or-more star filters

**Description:** Extend the pure filter modes and remappable key actions so
`Ctrl+Shift+1` through `Ctrl+Shift+5` select minimum ratings while the existing
`Shift+N` shortcuts remain exact.

**Acceptance criteria:**

- [ ] Every minimum mode includes N through 5 and excludes ratings below N.
- [ ] Exact-star filters are unchanged and all star filters AND-combine with
      recipe, tag, person, and focus filters.
- [ ] New modes survive folder reopen through the existing persisted text field.

**Verification:**

- [ ] Pure threshold matrix tests pass for all ratings and modes.
- [ ] Key parser tests prove shifted digit recovery with Ctrl held.
- [ ] Rust keymap tests prove new defaults coexist with old user overrides.

**Dependencies:** None

**Files likely touched:** `src/lib/order.ts`, `src/lib/order.test.ts`,
`src/App.tsx`, `src/lib/keys.test.ts`, `src-tauri/src/keymap.rs`, `SPEC.md`,
`CHANGELOG.md`

**Estimated scope:** Medium

## Checkpoint A: Culling semantics

- [ ] `npm test` passes.
- [ ] `npm run build` passes.
- [ ] Rating acknowledgement ordering and filter membership are reviewed.

## Task 3: Remove the warm-start filmstrip jump

**Description:** Initialize virtualization around the restored cursor, set the
real scroll position before paint, and eagerly request the already-small set of
mounted thumbnails.

**Acceptance criteria:**

- [ ] The first virtual window contains a saved cursor near either end.
- [ ] Reopening a warm folder shows the current row in the viewport without a
      visible row-zero flash or delayed lazy-load.
- [ ] No backend work is added to folder open or the flip path.

**Verification:**

- [ ] Pure virtual-window tests cover empty, first, middle, and final cursors.
- [ ] Runtime E2E checks current-row visibility and nonzero thumbnail width.
- [ ] Warm/cold current-thumbnail readiness is measured separately.

**Dependencies:** None

**Files likely touched:** `src/components/Filmstrip.tsx`, a colocated filmstrip
test, and an E2E spec

**Estimated scope:** Small

## Task 4: Add picture-only immersion mode

**Description:** Add a `Tab` action that suppresses chrome and presentation
overlays without mutating the user's panel/analysis preferences. `Escape`
restores the prior layout; errors remain visible.

**Acceptance criteria:**

- [ ] Tab leaves only the photograph plus any active safety/error notice.
- [ ] Navigation, rating, trash, and durability acknowledgements continue while
      immersed.
- [ ] Escape or Tab restores the exact previous filmstrip/dock/panel/overlay
      state; immersion never persists across launch.

**Verification:**

- [ ] Keymap/parser tests cover Tab and repeat suppression.
- [ ] E2E proves chrome visibility, culling keys, error visibility, and restore.
- [ ] Fit mode and locked zoom framing survive enter/exit.

**Dependencies:** Task 2 (final key-action inventory)

**Files likely touched:** `src/App.tsx`, `src/lib/viewer.ts`,
`src-tauri/src/keymap.rs`, UI/E2E tests, `SPEC.md`, `CHANGELOG.md`

**Estimated scope:** Medium

## Checkpoint B: Runtime behavior

- [ ] `npm test` and `npm run build` pass.
- [ ] Focused E2E for filmstrip and immersion passes.
- [ ] Storybook surfaces touched by the slice build cleanly.

## Task 5: Reorganize help and clarify sorting

**Description:** Present every live/remapped key once under intent-based
sections and replace internal sort words with Date captured, Filename, and
Rating. Keep `S` cycle and `Shift+S` reverse; add no new sort engine.

**Acceptance criteria:**

- [ ] Help sections are Navigate; Rate & recover; Filter & sort; Inspect photo;
      Panels & display; Folder & help, plus fallback Other.
- [ ] Every current binding appears exactly once and custom remaps remain live.
- [ ] Exact vs N-or-more modifiers and sort choices are immediately scannable.

**Verification:**

- [ ] Component/unit test proves complete, duplicate-free grouping.
- [ ] Storybook build passes and the rendered sheet is visually inspected.
- [ ] Accessibility E2E finds no new violations.

**Dependencies:** Tasks 1, 2, and 4

**Files likely touched:** `src/components/CheatSheet.tsx`, its test/story,
`src/App.css`, `src/App.tsx`, `src-tauri/src/keymap.rs`, `SPEC.md`,
`CHANGELOG.md`

**Estimated scope:** Medium

## Task 6: Full acceptance and local delivery

**Description:** Run the project-wide verification appropriate to hotkeys,
layout, startup rendering, and verdict behavior, then build, sign, install, and
launch the accepted app locally.

**Acceptance criteria:**

- [ ] Unit, TypeScript build, harness, Storybook, Rust test/fmt/clippy, E2E,
      accessibility, and durability checks pass.
- [ ] Normal and faces-active real-photo storms remain p99 <= 50 ms, zero
      steady-state miss serves, and cold open <= 1 s.
- [ ] Installed app's build stamp matches the final commit and the prior app is
      recoverable until launch verification succeeds.

**Verification:** The commands and measurements above are the task.

**Dependencies:** Tasks 1-5

**Files likely touched:** None beyond any test fixes required by the planned work

**Estimated scope:** Medium

## Checkpoint C: Complete

- [ ] All requested behavior is implemented and documented.
- [ ] Every check run and every check omitted is reported.
- [ ] Working tree is clean with small conventional commits.

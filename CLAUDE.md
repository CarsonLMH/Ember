@AGENTS.md

# Claude Code

`AGENTS.md` contains the shared Ember project context and invariants. Keep
Claude Code-only hooks, skills, permissions, and workflow guidance here or
under `.claude/`; do not duplicate the shared instructions.

## Skills

- `/design-audit <surface>` (`.claude/skills/design-audit/`) — audit one
  surface's design against the brief and the installed guideline skills, using
  the live `npm run tauri:mcp` build for screenshots and DOM measurements.
  Read-only on user data; publishes a findings report. `surfaces.md` in the
  skill lists every surface, how to reach it, and which keys are unsafe.

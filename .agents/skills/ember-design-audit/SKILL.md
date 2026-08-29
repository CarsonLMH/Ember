---
name: ember-design-audit
description: Audit one Ember UI surface against Ember's project design language using the live Tauri MCP build, DOM measurements, screenshots, and the shared evidence-report workflow. Use for Ember design audits and design reviews; do not use for implementing UI changes or performance gate runs.
---

# Ember design audit

This is the Codex entry point for Ember's shared design-audit workflow.

Before taking any audit action, read
`../../../.claude/skills/design-audit/SKILL.md` completely and follow it as the
canonical workflow. Resolve every path mentioned there from
`.claude/skills/design-audit/`.

The repository's `AGENTS.md` design language outranks every generic design
skill. An audit is read-only on user data, uses the MCP-enabled development
build only, and must never run alongside a performance gate.

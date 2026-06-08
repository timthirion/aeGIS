# `.claude/skills/`

Project-scoped skills for agents working on aeGIS.

A **skill** is a procedural recipe with embedded judgement —
the right defaults, the right failure-handling, the right
output shape — packaged so an agent can invoke it by name
instead of re-deriving the procedure each time. Skills aren't
just CLI aliases; if all the file would say is "run command
X," it doesn't earn its place here. The bar is: skills bundle
*knowledge*, not just incantations.

## Layout

Skills are **always** directory-based, even when they have
no supporting assets:

```
.claude/skills/
├── README.md              ← this file (convention)
└── <skill-name>/
    └── SKILL.md           ← the procedural recipe (required)
```

The flat-file form (`.claude/skills/<name>.md`) does **not**
register with the Claude Code harness — agents (under
`.claude/agents/`) use flat files; skills don't. Asymmetric
convention; both are real.

## Frontmatter

```yaml
---
name: skill-name
description: one-line summary used at selection time
version: 0.1.0
---
```

`name` is the identifier the agent invokes. `description` is
what the agent reads to decide whether the skill applies — be
concrete. `version` follows semver; bump when the procedural
recipe changes shape.

## When a skill earns its file

* Invoked **repeatedly**, in roughly the same shape every time.
* There's a **right way** to do it.
* The procedure embeds **judgement** — defaults, failure
  handling, retries.
* The recipe doesn't change often.
* The whole thing fits on **one page of markdown**.

If a candidate skill fails any of those, prefer one of:
- A plain shell command.
- A script in `scripts/`.
- An implementation plan in `plans/`.

## Currently scaffolded

- [`pre-flight/`](pre-flight/SKILL.md) — full quality-gate
  sequence required before any commit + push.
- [`commit-and-push/`](commit-and-push/SKILL.md) — pre-flight,
  then commit with project-convention message, push, watch CI
  on the just-pushed run.
- [`close-plan/`](close-plan/SKILL.md) — orchestrator for
  closing an implementation plan; invokes the review agents,
  refuses to close on unaddressed P0 attacks, ticks
  milestones, calls `commit-and-push`.

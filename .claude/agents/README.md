# `.claude/agents/`

Project-scoped agent role definitions for aeGIS. Where
`.claude/skills/` is **how** to do something procedurally,
`.claude/agents/` is **who** does it — a mandate, an input
contract, an output shape, and a list of anti-patterns the
role must refuse.

Agents here are **review-shaped, not implementer-shaped**.
They read code, plans, or renders and produce written reports.
They don't commit, push, or modify files. The implementer
agent (or human) decides what to do with the report.

## When an agent role earns its file

* The work has a **failure mode a single fresh agent
  consistently misses** — bias toward the artifact under
  review (the drafter doesn't see their own hand-waving), or
  toward subjective normalisation (a single visual judgement
  has high variance + high confidence, a dangerous mix).
* The mandate is **asymmetric**: the agent has to refuse to
  return "looks good" even when the artifact is strong.
* The role can be **invoked repeatedly** across many
  artifacts.

## Adversarial / paired roles

Agents that come in pairs (code-attacker + code-defender,
map-attacker + map-defender) have opposite mandates. **The
asymmetry is the entire point.** A single agent asked to
"review this and find issues" produces a balanced critique;
the attacker / defender pair produces a *sharper* one because
each agent's mandate prevents the hedging the single-agent
version drifts toward.

The caller is responsible for **synthesising the verdict** —
read the attacker's report, read the defender's response,
decide what to act on. No agent in the pair is allowed to
produce the verdict; their job is to argue, not to judge.

## Frontmatter

```yaml
---
name: agent-role-name
description: One-sentence mandate. Concrete enough that the matching call is unambiguous.
tools: Read, Bash, Grep, Glob
---
```

`tools` is an **explicit comma-separated list** of tool
names. The prose form `tools: All tools except X` is
display-only and the harness does NOT accept it. (We learned
this the hard way in quasi.)

The default tool list for review-shaped agents is
`Read, Bash, Grep, Glob` — enough to read code, plans,
renders, and run `git diff` / `git log` / `git show` without
modifying anything.

**Never include `Edit, Write, NotebookEdit, Agent` in a
review-shaped agent's tools list.** Edit / Write would let
the agent modify the artifact it's reviewing. `Agent` would
let it recurse, blowing the budget and tangling the synthesis
path.

## Required body sections

```
## Mandate
The one-sentence claim the agent must defend. Asymmetric for
adversarial roles.

## Inputs
What artifact the caller passes in. Be specific.

## Output shape
The structured form the report takes. Bullet lists, severity
tags, specific deliverables.

## Anti-patterns
The failure modes the agent must refuse.

## When to invoke
Concrete triggers.
```

## Skills × agents

A skill can invoke an agent as one of its steps. The
[`close-plan`](../skills/close-plan/SKILL.md) skill
orchestrates `plan-skeptic` + `code-attacker/defender` +
`map-attacker/defender` + `crs-skeptic` on every plan closure.

## Currently scaffolded

| Role | Mandate | Invoke when |
|------|---------|-------------|
| [`plan-skeptic.md`](plan-skeptic.md) | Find failure modes the plan doesn't address; find "Done when" criteria that don't actually deliver the goal. | Before closing an implementation plan; on any plan `active` >2 weeks. |
| [`code-attacker.md`](code-attacker.md) | Find concrete bugs, edge cases, race conditions, performance regressions, untested boundaries. | On plan-closing commits + any refactor over ~100 lines. |
| [`code-defender.md`](code-defender.md) | Accept the real attacks, refute the misreadings, defer the rest. | Paired with `code-attacker`; never alone. |
| [`map-attacker.md`](map-attacker.md) | Find visual regressions in map renders — label collisions, projection wobble, tile seams, missing features. | When re-rendering reference fixtures. |
| [`map-defender.md`](map-defender.md) | Find visual improvements in the new render. | Paired with `map-attacker`; never alone. |
| [`crs-skeptic.md`](crs-skeptic.md) | Find lon/lat-ordering bugs, lost EPSG metadata, datum confusion, projection-range overflows. | On any diff touching CRS, projection, or coordinate-math code. |

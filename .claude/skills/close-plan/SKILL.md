---
name: close-plan
description: Close an implementation plan (plans/NNNN-*.md) by orchestrating the review agents, refusing to close on unaddressed P0 attacks, ticking all milestones, and committing+pushing the closure. Composes pre-flight + commit-and-push + plan-skeptic + code-attacker/defender + map-attacker/defender + crs-skeptic. Research plans (RNNNN-*.md) have a separate closure path.
version: 0.1.0
---

# Close plan

The orchestrator for plan closure. Where `commit-and-push` handles the
*push* half of any unit of work, `close-plan` handles the *was-this-
actually-finished?* half — the review gauntlet that the agent roles
exist to run.

A plan that flips to `done` without this skill's gauntlet risks
shipping with a hand-waved milestone, a P0 attack nobody surfaced, a
silent CRS regression, or a map render that the agent normalised away
as "looks fine."

## Inputs

* A plan path or number: `0001` or `plans/0001-foundation.md`.
* The current branch should be the one carrying the plan-closing
  commits (typically `main`).

## The sequence (defaults)

1. **Read the plan.** Confirm `Status:` is currently `proposed` or
   `active`. If already `done`, refuse and surface the existing status.
   If `abandoned`, refuse (abandoned plans don't get closed; they get
   archived).

2. **Identify the plan-closing commit(s).** Use
   `git log --oneline --first-parent -- plans/<plan>.md` to find the
   commits that touched the plan file; the most recent one is usually
   the plan-closing edit.

3. **Invoke [`plan-skeptic`](../../agents/plan-skeptic.md)** on the
   plan file. Surface the full report.

4. **Inspect the diff for code changes.** If the plan-closing commits
   touch `.rs`, `.wgsl`, `.toml`, or `.py` files (i.e. real code, not
   just plan markdown + new reference renders + new data fixtures),
   invoke the [`code-attacker`](../../agents/code-attacker.md) +
   [`code-defender`](../../agents/code-defender.md) pair on the diff
   range. Surface both reports.

5. **Inspect the diff for CRS / projection / coordinate-math
   changes.** If the diff touches `core::crs::*`, `core::tile`, or any
   file with `proj`, `mercator`, `transform`, `lonlat`, `epsg`,
   `wgs84` in its path, invoke
   [`crs-skeptic`](../../agents/crs-skeptic.md) on the diff. CRS bugs
   are this project's silent-killer failure mode; this gate is
   load-bearing.

6. **Inspect the diff for map-render output changes.** Reference-image
   fixtures live under `data/reference/`. If any of these PNGs changed
   in the plan-closing commit, materialise the prior committed version
   (`git show HEAD~1:<path> > /tmp/old.png`) and invoke
   [`map-attacker`](../../agents/map-attacker.md) +
   [`map-defender`](../../agents/map-defender.md) on each (old, new)
   pair. Surface all reports.

7. **Surface the synthesised verdict.** Read all the agent reports;
   identify every P0 attack from `plan-skeptic`, `code-attacker`,
   `crs-skeptic`, and `map-attacker`; identify every P0 attack the
   `code-defender` or `map-defender` accepted. Present as:

   ```
   close-plan synthesis on <plan>:
     plan-skeptic:    <N>×P0, <N>×P1
     code-attacker:   <N>×P0, <N>×P1
     code-defender:   accepted <N> P0, refuted <N>, deferred <N>
     crs-skeptic:     <N>×P0, <N>×P1 (or "skipped — no CRS diff")
     map-attacker:    <N>×P0, <N>×P1 (per fixture pair)
     map-defender:    <N> headline improvements
   ```

8. **Refuse to close on unaddressed P0.** If any P0 attack survived
   the defender's response, the skill refuses to flip status to
   `done`. The caller addresses the attack (fixes code, re-renders,
   edits the plan) and re-invokes.

9. **On approval — explicit only.** Once the human (or the calling
   agent with appropriate authority) approves the closure, **only
   then**:
   * Tick all `[ ]` to `[x]` in the plan's milestone sections.
   * Change `Status:` to `done`.
   * Update `Last updated:` to today's ISO date.
   * Update `Last touched on:` to a short description of the closing
     pass.

10. **Invoke [`commit-and-push`](../commit-and-push/SKILL.md)** with
    the plan-closure edit. The commit message references the closed
    plan + the agent reports' headline findings.

11. **Update auto-memory.** Append a one-line entry to the project's
    memory naming the closed plan + its headline outcome. Memory drift
    on closed plans is a recurrent auto-memory-staleness failure mode.

## Skipping or extending the defaults

Plans can declare exceptions in their `Done when` section:

* **"close-plan must invoke `ultrareview`"** — for plans that touch
  shader math or projection math, run an `ultrareview`-style cloud
  agent (when invoked by the user) in addition to the default pair.
* **"close-plan may skip map-attacker/defender"** — for plans that
  don't change visual output (CI workflow plans, README rewrites).
* **"close-plan may skip crs-skeptic"** — for plans that don't touch
  any coordinate math (UI-only changes, format-decode plans for
  non-spatial fields).

Default behaviour applies when the plan declares no exceptions.

## Refusal conditions

Hard refusals (the skill does **not** ask permission to override):

* Plan status is already `done` or `abandoned`.
* The plan-closing diff doesn't exist (no commit has touched the
  plan file recently).
* Any P0 attack from `plan-skeptic`, `code-attacker`, `crs-skeptic`,
  or `map-attacker` is unaddressed.

Soft refusals (the skill flags but the caller can override):

* The `Done when` criteria look only partially satisfied.
* The plan was never `active` (jumping from `proposed` to `done`
  skips an intended workflow stage).

## Output shape

On a clean close:

```
close-plan <plan> on <branch>:
  ✓ plan-skeptic: 0 P0, 2 P1 (both pre-acknowledged in plan)
  ✓ code-attacker/defender: 1 P0 raised, defender refuted (cited
    core/tile.rs:142), 3 P1 deferred to plan 00NN
  ✓ crs-skeptic: no P0; 1 P1 (lon/lat ordering at a public boundary)
  ✓ map-attacker/defender (2 fixture pairs):
    - basemap_paris_z14.png: no regressions; 1 improvement
    - overlay_countries_world_z3.png: no changes detected
  ✓ ticked 14 milestones; status → done
  ✓ commit-and-push: CI green (run <id>, <duration>)
Done.
```

On refusal (unaddressed P0):

```
close-plan <plan>: REFUSED
  ✗ crs-skeptic raised P0 at core/crs/mercator.rs:88 ("forward()
    swaps lon/lat for inputs above |lat| > 85° without erroring");
    no defender response yet.
Action: address the P0 (fix the code OR document the bound),
re-pre-flight, re-invoke close-plan.
```

## Implementation vs research plans

This skill targets **implementation plans** (`plans/NNNN-*.md`).
Research plans (`plans/research/RNNNN-*.md`) have different closure
semantics:

* Status moves to `accepted` (paper accepted) or `abandoned`.
* The `Findings` section must carry at least one entry before
  closure.
* Defaults: invoke `research-critic` (when it exists) instead of
  `plan-skeptic`.

A future `close-research-plan` skill (not scaffolded today) would
mirror this skill's shape for the research case.

## What this skill is NOT

* Not a code review for routine commits. Plain commits go through
  `commit-and-push`. `close-plan` is the review-gauntlet for closing
  a plan.
* Not an unconditional approve-and-merge. The refusal conditions are
  load-bearing.
* Not a substitute for human judgement. The agents produce reports;
  the human renders the verdict. The skill enforces process, not
  taste.

## When to invoke

* When a plan's milestones are all implemented and the closing commit
  is the next intended action — before the closing edit, not after.
* When a plan has been `active` for ≥2 weeks and the caller wants
  to audit "is this actually finished?" The skill produces a
  diagnostic report even when it refuses to close.

Don't invoke on `proposed` plans that haven't been implemented yet —
the agents won't have a diff to attack and the report is empty.

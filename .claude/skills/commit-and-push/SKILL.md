---
name: commit-and-push
description: Run pre-flight, construct a project-convention commit message, commit, push to origin, watch CI on the just-pushed run, refuse to consider the push "done" until CI is green.
version: 0.1.0
---

# Commit + push

The closing half of any unit of work. `pre-flight` gates *whether* to
commit; this skill takes a green pre-flight, constructs the commit,
pushes it, and stays attached until CI confirms green.

The autonomy bargain: **commit and push freely, but CI must stay
green** — this skill enforces the second half mechanically.

## The sequence

1. **Invoke [`pre-flight`](../pre-flight/SKILL.md).** If red, stop.
   Surface the pre-flight failure verbatim. **Do not commit.**

2. **Stage the intended files.** `git add -A` if everything in the
   working tree should ship; `git add <paths>` when only a subset
   should. Prefer the explicit-paths form when the working tree has
   uncommitted experiments next to the intended change.

3. **Construct the commit message** per the project convention (below).
   Use a HEREDOC to pass the message so the body formatting survives.

4. **Commit.** If the commit fails (pre-commit hook, missing sign-off),
   surface the error. Do **not** retry with `--no-verify`.

5. **Push to origin** (or the active branch). If push is rejected for
   non-fast-forward, **do not auto-force**. Surface the rejection — it
   likely means upstream moved and the human (or a `rebase-and-retry`
   skill) decides next.

6. **Watch CI on the new commit.**
   `gh run watch $(gh run list --limit 1 --json databaseId
   --jq '.[0].databaseId') --exit-status`. Confirm the run is for
   the just-pushed SHA, not a stale older one.

7. **On CI green:** report the run ID + duration. The push is done.

8. **On CI red:** fetch the failing job's log (`gh run view <id>
   --log-failed`), surface the root error verbatim, and treat the
   push as **not done**.

## Commit message convention

Subject (line 1): present-tense, ≤ 72 chars, no trailing period.
Typical shapes:

```
MAP-tiles: async fetcher + LRU cache + textured-quad pass
CRS-mercator: round-trip stable to 1e-9 across a 360×170 grid
FMT-geojson: round-trip preserves nested feature properties
plans: tick 0001 M2; record OSM-tile policy resolution
```

Body (after blank line): why the change exists, what the trade-offs
are. Reference plans (`plan 0001 MAP-tiles`), commits (`commit abc1234`),
and memory entries (`[[name]]`) by exact identifier. Wrap body at ~72
chars but don't be religious — `wgpu`, `cargo`, and URL-shaped lines
can spill.

Footer (after blank line): always include the Co-Authored-By line:

```
Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
```

The full pattern, ready to pipe:

```bash
git commit -m "$(cat <<'EOF'
Subject line ≤ 72 chars

Body paragraph explaining the why. Reference plans, commits, and
memory entries by exact identifier. Don't summarise the diff; the
diff is the diff.

Trade-offs paragraph (when there are any). What we chose, what we
explicitly didn't, what the failure mode would be if the choice was
wrong.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

## Failure handling

### Pre-flight red

Surface the failure verbatim; do not commit; do not attempt to fix the
underlying issue silently. The caller addresses and re-invokes.

### Push rejected

Most commonly: upstream moved (non-fast-forward). Surface the error; do
**not** force-push. Correct response: `git pull --rebase`, re-run
pre-flight on the rebased tree, then re-invoke.

Force-push to `main` is **never** safe under autonomy mode.

### CI red post-push

Fetch `gh run view <id> --log-failed`. If the failure is fmt drift
(pre-flight should have caught it — investigate why it didn't), retry
with `cargo fmt` + fresh `commit-and-push`. Otherwise surface verbatim
and stop. **Do not auto-commit a fix.**

### Push succeeded, CI still queued/in-progress

`gh run watch` blocks until the run completes. Don't exit early.

## Output shape

On full green:

```
commit-and-push on <branch>:
  ✓ pre-flight green
  ✓ committed <SHA> ("<subject>")
  ✓ pushed to origin/<branch>
  ✓ CI green (run <id>, <duration>)
Done.
```

On any red, surface the failure verbatim and stop.

## What this is NOT

* Not a generic git wrapper. The skill earns its file by *bundling
  pre-flight + push + CI confirmation*. Stripping any one step
  collapses the value.
* Not the place for `--amend`. Amending a pushed commit requires
  force-push.
* Not the place for branch creation or tagging.

## When to invoke

Whenever a logical unit of work is complete. Common callers:
- A milestone-closing edit-and-test loop ("MAP-tiles landed, push it").
- A `close-plan` invocation needing to land the plan-status edit.

Don't invoke after every single file write. The unit of work should be
a coherent commit — the agent's discretion. Green CI is the proof the
commit was the right unit.

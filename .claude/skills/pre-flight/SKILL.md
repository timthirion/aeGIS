---
name: pre-flight
description: Run the full quality-gate sequence (fmt-check, clippy -D warnings, wasm32 check, all-targets tests, Python tests if scripts/ touched). Auto-fix fmt drift once; surface other failures verbatim.
version: 0.1.0
---

# Pre-flight quality gates

The full sequence required before any commit + push. Every CI red on a
Rust+wasm repo has the same shape: one of these four gates would have
caught it locally.

## The sequence

Run these four, **in order**, halting on the first failure:

1. `cargo fmt --all -- --check`
2. `cargo clippy --all-targets -- -D warnings`
3. `cargo check --target wasm32-unknown-unknown --lib`
4. `cargo test --all-targets`

If the working tree touches `scripts/` (any file under that directory),
also run:

5. `python3 -m unittest discover scripts -p 'test_*.py' -v`

## Failure handling

### Step 1 (fmt) — auto-fix once, then re-check

`cargo fmt --check` failing means rustfmt would reformat something. The
right move is to run `cargo fmt --all`, then re-run `cargo fmt --all --
--check` to verify it's clean. **Do this exactly once.** If `--check`
still fails after the auto-format, something is structurally wrong
(likely an `rustfmt::skip` directive on a file that doesn't satisfy
the rule even formatted); surface the diff verbatim and stop.

### Step 2 (clippy) — surface verbatim, never auto-fix

Clippy failures are code issues, not style drift. Do **not** run
`cargo clippy --fix`. Surface the error verbatim including the
`file:line` and the suggested fix. The human (or agent re-entering)
decides whether to apply the suggestion or rewrite.

### Step 3 (wasm32) — distinguish target-missing from real error

If the error message contains "target `wasm32-unknown-unknown` may not
be installed", the fix is `rustup target add wasm32-unknown-unknown` —
not a code change. Surface as a setup issue, not a build failure.

For other wasm32 errors, surface verbatim. The most common real failure:
a `cfg(not(target_arch = "wasm32"))` boundary getting missed when a
native-only crate (e.g. `winit` features, `image` with native codecs,
or any C-binding crate like `proj`) creeps into the wasm-visible code
path. aeGIS's pure-Rust-by-default discipline (see `AGENTS.md`) is the
upstream defence; this gate is the downstream catch.

### Step 4 (test) — surface verbatim with the failure summary

`cargo test --all-targets` failures need the test name + the assertion
message + the location. Surface the FAILED lines + the panic message for
each failing test. Don't truncate.

aeGIS-specific high-value failure surfaces:
- CRS round-trip tests: if a `mercator::forward(mercator::inverse(p))`
  test fails, the epsilon was tightened or a math change broke
  invertibility — surface the exact `(lon, lat)` that diverged.
- Format I/O round-trip tests: surface the geometry / property that
  failed to round-trip.

### Step 5 (Python, conditional) — surface verbatim

Same shape. The Python suite is small enough that default `unittest`
output is fine in full.

## Output shape

Per-step ✓ / ✗ summary. On ✗, include the failing step's full output.
On ✓ across all steps, a one-line "pre-flight green" is enough.

```
Pre-flight on <branch>:
  ✓ fmt
  ✓ clippy
  ✓ wasm32 check
  ✓ tests (N passing across M suites)
  ✓ python tests (skipped — no scripts/ changes)
Pre-flight green.
```

## What pre-flight is NOT

* Not a commit step. Pre-flight gates *whether* to commit; it doesn't
  commit. `commit-and-push` calls pre-flight as its first action.
* Not a "fix things" loop. The fmt auto-fix is the **only** auto-
  correction. Everything else is surface-and-stop.
* Not a substitute for CI. CI runs these same checks on a clean Linux
  runner; pre-flight catches issues before the push so CI stays green
  by default.

## When to invoke

Before any `git commit`. Before any `git push` that follows a commit
you didn't pre-flight. After any non-trivial refactor.

Skip is fine for trivial commits (a README typo) — but the moment the
change touches code, pre-flight should run.

# Research-only freeze — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development or executing-plans.

**Goal:** Implement spec docs/superpowers/specs/2026-08-22-research-only-freeze-design.md — hard locks in code: live refused until stored PASS; new variant hashes refused without explicit `NEW-OOS` unlock.

**Spec source of truth:** docs/superpowers/specs/2026-08-22-research-only-freeze-design.md (§4, §5, §7, §8, §10). Read it first — its §8 test list is binding.

## Global Constraints

- `trade --live` with verdict != Some(true): exit non-zero BEFORE any stdin read; message names the freeze + current banner text. `--live --dry-run` identical refusal.
- Stored PASS keeps today's GO prompt (`go`/empty/`NEW-OOS` are not `GO`).
- `flatten --live` UNCHANGED — kill switch still reaches GO prompt even on FAIL.
- `search`: NEW hash without `--unlock-new-study` → whole command fails on FIRST new hash, before `evaluate_config`/record; error names reserved slots + how to unlock. Known hashes replay free. Unlock = flag + stdin literal `NEW-OOS` (trimmed, exact case); anything else aborts. Cap 20 still enforced after unlock. Unlock never persists.
- No key/secret material in errors. anyhow bails, no panics.

### Task 1: Guards + wiring + docs

**Files:** Modify `src/main.rs`; Modify `docs/RUNBOOK.md`; Create/modify `README.md`; Test: inline

**Interfaces:**
```rust
pub fn ensure_live_unlocked(verdict: Option<bool>) -> anyhow::Result<()>  // Some(true)=>Ok; else bail naming freeze
pub fn ensure_search_allowed(is_new_hash: bool, unlocked: bool) -> anyhow::Result<()> // known=>Ok; new+unlocked=>Ok; else bail w/ reservation msg
fn unlock_confirmed(stdin_line: &str) -> bool // trim == "NEW-OOS"
// Search subcommand gains #[arg(long)] unlock_new_study: bool (NOT on Trade)
// run_trade live branch: ensure_live_unlocked(verdict)? BEFORE confirm_live
// run_search: resolve unlock (flag => read stdin line once => unlock_confirmed), then per-variant ensure_search_allowed(!known.contains(&hash), unlocked)?
```
- [ ] Step 1: failing tests for every §8 row reachable at unit level (guards) + clap parse: `--unlock-new-study` accepted on search, rejected on trade.
- [ ] Step 2: implement guards + wire both commands; Step 3: full `cargo test` green + clippy/fmt clean; Step 4: docs (README status line; RUNBOOK §0 "blocked in code"); Step 5: commit `Freeze: live hard-refuse until PASS; search new-hash reservation with NEW-OOS unlock`

# Research-only freeze — Design Spec

**Date:** 2026-08-22  
**Status:** Draft for user review  
**Supersedes:** live rollout in `2026-08-21-ai-trading-platform-design.md` §10–§11 until a stored gate PASS exists. Gate thresholds in §5 are unchanged.

## 1. Goal

This repo is a **measurement lab**, not an income system. Money is the long-term aim; it is not the current operating mode.

Until a **new** hypothesis on a **new** out-of-sample window stores an overall gate PASS, the binary must make the failed study cheap to replay and expensive to fake.

This freeze does not create edge. It prevents spending the remaining variant budget and real funds on the study that already failed.

## 2. Decisions

| Decision | Choice |
|---|---|
| Venue | Binance spot only. No Polymarket (or any second venue) in this freeze. |
| Current EMA/RSI study | Closed. Recorded overall verdict is FAIL / NO-GO. |
| Variant budget | 12 distinct hashes used, cap 20. **8 reserved.** |
| New hashes on this window | Refused by default. |
| Live trading | Hard-refused until a stored overall PASS. `GO` cannot override FAIL or missing verdict. |
| Testnet / dry-run | Allowed for **engine** checks. Fills never count toward the gate. |
| `backtest` | Allowed. Does not write `backtest_runs` and does not charge the budget (already true). |
| `search` replay of the known 12 | Allowed. Re-runs are free. |

## 3. Facts on disk (do not “fix” by searching more)

As of 2026-08-22:

- `config_state.variant_budget_used` = 12
- 12 distinct `backtest_runs.config_hash` values
- No config meets the frozen §5 gate (PF ≥ 1.30, ≥ 3 profitable pairs, OOS DD < 20%, ≥ 20 OOS trades per pair)
- Typical OOS sample on this window is well below 20 trades per pair

Re-running the existing 2×3×2 EMA/RSI grid is a replay. Changing RSI/ATR/RR, pairs, timeframe, or strategy name produces a **new hash** and would spend reserved slots.

## 4. Live hard-refuse

Today `trade --live` and `flatten --live` print the NO-GO banner and still continue if stdin is the literal word `GO`. That is a warning, not a lock.

**New rule for `trade`:**

1. Resolve the latest stored overall search verdict the same way `latest_search_overall_verdict` already does (newest `config_hash` by `ran_at`, then `evaluate_gate` on that hash’s OOS rows).
2. If `--live` is set and the verdict is not `Some(true)`, exit non-zero **before** reading stdin. Do not prompt for `GO`. Message must say live is refused until a stored overall PASS, and must include the banner text (NO-GO or no result on record).
3. `--live` plus `--dry-run` is still `--live`: refused without PASS. Dry-run is not a back door onto `api.binance.com`.
4. If the verdict is `Some(true)`, keep today’s `GO` prompt. PASS does not auto-start live.

**`flatten --live`:** still allowed without PASS. Flatten is a kill switch (cancel → confirm empty → market-reduce). It still prints the banner and still requires `GO` on live. Testnet `flatten` is unchanged.

**`trade` without `--live`:** still defaults to testnet. No new confirmation.

## 5. Variant-budget lock

The cap of 20 remains. This freeze adds a second gate **in front of** that cap.

`check_variant_budget` (or a wrapper next to it) must refuse a **new** hash unless the operator unlocks this one `search` invocation.

Default (no unlock):

- Known hash → run, do not increment `variant_budget_used`
- New hash → exit non-zero **before** `evaluate_config` / `record_backtest_results`. Budget and `backtest_runs` unchanged. Error must say the remaining slots are reserved for a documented new OOS study, and name how to unlock. The whole `search` command fails on the first such hash (no “skip and keep going”). Known hashes already replayed earlier in that same run are fine; they did not charge the budget.

Unlock (same invocation only; do not persist “unlocked”):

- `search --unlock-new-study`
- stdin must be the literal word `NEW-OOS` (trim, exact case). Anything else aborts.
- Then existing cap logic applies: new hashes allowed until 20 distinct; the 21st is still refused.

No sqlite “lock bit” that can be left off. If they forget the flag, the 8 stay unused.

`backtest` never records hashes today; do not start recording them in this freeze.

## 6. Unlock is not a strategy change

`--unlock-new-study` is a seatbelt release, not permission to mine this same 70/30 split. A real new study still requires, **before** that flag is used:

- A written hypothesis (new spec or a section added to this one)
- An OOS window that was not used to score the failed EMA/RSI grid (time has passed and the split is pre-declared, or the window is otherwise unused)
- The same frozen §5 numeric thresholds unless a new spec explicitly replaces them **before** seeing results

This implementation plan does not include that new study. It only implements the seatbelts.

## 7. Docs

- README status line: research-only; live refused; 8 variants reserved.
- RUNBOOK §0: live is **blocked in code** until PASS, not merely “not advised.” Testnet remains the sanctioned engine stage.
- `.env.example` unchanged (keys still required for testnet `trade` / `flatten`).

## 8. Tests (must exist before the behavior is considered done)

- `trade --live` with stored FAIL → non-zero, no dependence on stdin `GO`.
- `trade --live` with empty `backtest_runs` → non-zero, no `GO` prompt path.
- `trade --live` with stored PASS → still requires literal `GO`; `go` / empty / `NEW-OOS` are not `GO`.
- `trade --live --dry-run` with FAIL → refused (same as live).
- `trade` testnet / `--dry-run` without `--live` → no live refuse (keys may still be missing; that is a different error).
- `flatten --live` with FAIL → still reaches the `GO` prompt path (kill switch).
- `check_variant_budget` / search: new hash without unlock → err; `variant_budget_used` unchanged.
- Known hash with lock → ok.
- Unlock phrase: `NEW-OOS` allows new hash (subject to cap 20); `new-oos` / `GO` / empty do not.
- Cap 20 still refuses the 21st distinct hash even with unlock.

CLI parse tests for `--unlock-new-study` on `search` only (not on `trade`).

## 9. Non-goals (this freeze)

- Polymarket or any multi-venue work
- Making EMA/RSI pass; changing gate numbers after seeing results
- Wiring the unused WS/cache/bus into `trade`
- launchd, sleep disable, 24/7 hosting
- Hard-blocking testnet
- Resetting `variant_budget_used`
- Dashboard, shadow mode, Telegram heartbeat

## 10. Error handling

- Live refuse and budget refuse are `anyhow` errors to the CLI (non-zero exit), not panics.
- Messages must not print API keys, wallet keys, or `.env` contents.
- A refused `search` must not write partial `backtest_runs` for the new hash (refuse before record, same as today’s cap check).

## 11. Rollout

1. Tests first (failing), then the two guards, then README/RUNBOOK.
2. No live Binance calls in CI.
3. After merge, the operator can `fetch` / `backtest` / replay `search` without burning the 8.

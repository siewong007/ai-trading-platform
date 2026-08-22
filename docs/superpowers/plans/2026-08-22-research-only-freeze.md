# Research-only freeze Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Seatbelt the failed EMA/RSI study: hard-refuse `trade --live` until a stored overall gate PASS, and refuse new search hashes unless this invocation is unlocked with `NEW-OOS`, preserving the remaining 8 variant slots.

**Architecture:** Two pure guards in `src/main.rs` next to the existing `confirm_live` / `check_variant_budget` helpers. `run_trade` calls the live guard before stdin. `search` gains `--unlock-new-study`; `run_search` confirms `NEW-OOS` then passes an `unlocked` bool into the budget check. No new crates, no sqlite lock bit, no Polymarket, no strategy changes.

**Tech Stack:** existing clap / anyhow / tokio tests in `src/main.rs`; `cargo test` from the repo root (target-dir is already redirected by `.cargo/config.toml`).

## Global Constraints

- Frozen §5 gate numbers stay exactly `GATE_MIN_PF=1.30`, `GATE_MIN_PROFITABLE_PAIRS=3`, `GATE_MAX_DD_PCT=20.0`, `GATE_MIN_TRADES_PER_PAIR=20`, `GATE_MAX_VARIANTS=20`
- Live refuse if `latest_search_overall_verdict` is not `Some(true)`; do not read stdin in that path
- `--live --dry-run` is still live
- `flatten --live` remains a kill switch (banner + `GO`); do not apply the new live refuse
- New search hash without unlock: fail the whole command before `evaluate_config` / `record_backtest_results`
- Unlock is per invocation (`--unlock-new-study` + stdin `NEW-OOS`); do not persist unlocked state
- Cap 20 still applies after unlock; the 21st distinct hash is still refused
- `backtest` still must not write `backtest_runs`
- Errors are `anyhow` to the CLI (non-zero exit), never panics; never print secrets
- Every task ends `cargo test` green and committed
- No live Binance calls in tests or CI

**Spec:** `docs/superpowers/specs/2026-08-22-research-only-freeze-design.md`

**Files:**
- Modify: `src/main.rs` (helpers, clap `Search`, `run_trade`, `run_search`, tests)
- Modify: `README.md`, `docs/RUNBOOK.md`, `scripts/smoke_serious.sh`
- Do not create new modules

---

### Task 1: Hard-refuse `trade --live` without stored PASS

**Files:**
- Modify: `src/main.rs` (`confirm_live` neighbors, `run_trade`, `mod tests`)

**Interfaces:**
- Consumes: existing `gate_banner(latest_pass: Option<bool>) -> String`, `confirm_live`, `BINANCE_BASE`
- Produces:
```rust
fn live_trade_permitted(latest_pass: Option<bool>) -> bool
fn refuse_live_trade(latest_pass: Option<bool>) -> anyhow::Result<()>
```
`live_trade_permitted` is `true` only for `Some(true)`. `refuse_live_trade` returns `Ok(())` when permitted; otherwise `Err` whose display contains `gate_banner(latest_pass)` and the exact substring `live trading refused until a stored overall gate PASS exists`. It must not read stdin.

- [ ] **Step 1: Write the failing tests** in `src/main.rs` `mod tests` (do not implement the helpers yet):

```rust
#[test]
fn live_trade_permitted_only_on_stored_pass() {
    assert!(!live_trade_permitted(None));
    assert!(!live_trade_permitted(Some(false)));
    assert!(live_trade_permitted(Some(true)));
}

#[test]
fn refuse_live_trade_errors_on_fail_and_missing_without_needing_go() {
    for v in [None, Some(false)] {
        let err = refuse_live_trade(v).unwrap_err().to_string();
        assert!(err.contains("live trading refused until a stored overall gate PASS exists"), "{err}");
        assert!(err.contains(&gate_banner(v)), "{err}");
        assert!(!err.to_lowercase().contains("api_key"), "{err}");
        assert!(!err.to_lowercase().contains("secret"), "{err}");
    }
}

#[test]
fn refuse_live_trade_ok_on_pass_so_go_prompt_can_run() {
    assert!(refuse_live_trade(Some(true)).is_ok());
}
```

Keep the existing `live_confirmation_requires_the_literal_word_go` test; PASS still requires `GO`.

- [ ] **Step 2: Run tests to verify they fail**

```bash
cd "/Volumes/APPLE EXTERNAL SSD /Personal Projects/ai-trading-platform"
cargo test live_trade_permitted_only_on_stored_pass refuse_live_trade -- --exact
```

Expected: compile FAIL (`cannot find function live_trade_permitted` / `refuse_live_trade`).

- [ ] **Step 3: Minimal implementation**

Place next to `confirm_live`:

```rust
fn live_trade_permitted(latest_pass: Option<bool>) -> bool {
    latest_pass == Some(true)
}

fn refuse_live_trade(latest_pass: Option<bool>) -> anyhow::Result<()> {
    if live_trade_permitted(latest_pass) {
        return Ok(());
    }
    anyhow::bail!(
        "{}\nlive trading refused until a stored overall gate PASS exists",
        gate_banner(latest_pass)
    );
}
```

In `run_trade`, after `let verdict = db.latest_search_overall_verdict().await?;` and `println!("{}", gate_banner(verdict));`, change only the live branch:

```rust
    if base == BINANCE_BASE {
        refuse_live_trade(verdict)?;
        confirm_live(verdict)?;
    }
```

Do **not** call `refuse_live_trade` from `run_flatten`. Flatten keeps `println!(banner)` + `confirm_live` when `base == BINANCE_BASE`.

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test live_trade_permitted_only_on_stored_pass refuse_live_trade live_confirmation_requires_the_literal_word_go -- --exact
cargo test
```

Expected: all green.

- [ ] **Step 5: Commit**

```bash
git add src/main.rs
git commit -m "Task 1: refuse trade --live until stored gate PASS"
```

---

### Task 2: Reserve remaining variant slots (lock + per-run unlock)

**Files:**
- Modify: `src/main.rs` (`Command::Search`, `main` match, `check_variant_budget`, `run_search`, `mod tests`)

**Interfaces:**
- Consumes: Task 1 unchanged; existing `known.contains(&hash)`, `GATE_MAX_VARIANTS`
- Produces:
```rust
fn confirm_new_study_input(input: &str) -> bool  // trim == "NEW-OOS"
fn confirm_new_study() -> anyhow::Result<()>     // stdin, same shape as confirm_live
fn check_variant_budget(
    used_distinct: u32,
    is_new_hash: bool,
    new_study_unlocked: bool,
) -> anyhow::Result<()>
```

`Command::Search` gains `unlock_new_study: bool` via `#[arg(long)]` (clap name `--unlock-new-study`).

`run_search(config_path: &str, unlock_new_study: bool)`: if `unlock_new_study`, call `confirm_new_study()?` once at the start; then for every variant call `check_variant_budget(used, !known.contains(&hash), unlocked)?` **before** `evaluate_config`. First failure aborts the command. Do not persist unlock.

Lock error (new hash, `new_study_unlocked == false`) must contain `reserved` and `--unlock-new-study` and `NEW-OOS`. Cap error (new hash, unlocked, `used_distinct >= GATE_MAX_VARIANTS`) must still contain `budget` (keep today’s wording).

- [ ] **Step 1: Write the failing tests** — replace the old two-arg budget test with these (the two-arg call will not compile until Step 3):

```rust
#[test]
fn variant_budget_lock_refuses_new_hash_without_unlock() {
    let err = check_variant_budget(12, true, false).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("reserved"), "{msg}");
    assert!(msg.contains("--unlock-new-study"), "{msg}");
    assert!(msg.contains("NEW-OOS"), "{msg}");
}

#[test]
fn variant_budget_lock_allows_known_hash_without_unlock() {
    assert!(check_variant_budget(12, false, false).is_ok());
    assert!(check_variant_budget(GATE_MAX_VARIANTS, false, false).is_ok());
    assert!(check_variant_budget(u32::MAX, false, false).is_ok());
}

#[test]
fn variant_budget_unlock_allows_new_hash_under_cap() {
    assert!(check_variant_budget(GATE_MAX_VARIANTS - 1, true, true).is_ok());
}

#[test]
fn variant_budget_unlock_still_refuses_21st_distinct_hash() {
    let err = check_variant_budget(GATE_MAX_VARIANTS, true, true).unwrap_err();
    assert!(err.to_string().contains("budget"), "{err}");
}

#[test]
fn new_study_unlock_requires_the_literal_phrase() {
    assert!(!confirm_new_study_input("new-oos\n"));
    assert!(!confirm_new_study_input("GO\n"));
    assert!(!confirm_new_study_input(""));
    assert!(!confirm_new_study_input("NEW-OOS extra\n"));
    assert!(confirm_new_study_input("NEW-OOS\n"));
    assert!(confirm_new_study_input("NEW-OOS"));
}

#[test]
fn search_subcommand_parses_unlock_flag() {
    let cli = Cli::try_parse_from(["tp", "search", "--config", "c.toml"]).unwrap();
    match cli.command {
        Command::Search {
            config,
            unlock_new_study,
        } => {
            assert_eq!(config, "c.toml");
            assert!(!unlock_new_study);
        }
        _ => panic!("expected Search"),
    }
    let cli = Cli::try_parse_from(["tp", "search", "--unlock-new-study"]).unwrap();
    match cli.command {
        Command::Search {
            unlock_new_study, ..
        } => assert!(unlock_new_study),
        _ => panic!("expected Search"),
    }
    assert!(
        Cli::try_parse_from(["tp", "trade", "--unlock-new-study"]).is_err(),
        "unlock flag is search-only"
    );
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test variant_budget_lock new_study_unlock search_subcommand_parses_unlock_flag
```

Expected: compile FAIL (unknown items / `Search` has no `unlock_new_study` / `check_variant_budget` arity).

- [ ] **Step 3: Minimal implementation**

Clap:

```rust
    Search {
        #[arg(long, default_value = "config/strategy_ema_rsi.toml")]
        config: String,
        /// Allow new config hashes this run (requires typing NEW-OOS)
        #[arg(long)]
        unlock_new_study: bool,
    },
```

`main` match: `Command::Search { config, unlock_new_study } => rt.block_on(run_search(&config, unlock_new_study))?`

Helpers (same stdin pattern as `confirm_live`):

```rust
fn confirm_new_study_input(input: &str) -> bool {
    input.trim() == "NEW-OOS"
}

fn confirm_new_study() -> anyhow::Result<()> {
    println!("Unlocking reserved variant slots. Type NEW-OOS to continue:");
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    anyhow::ensure!(
        confirm_new_study_input(&line),
        "aborted: new configs require the literal phrase NEW-OOS on stdin"
    );
    Ok(())
}

fn check_variant_budget(
    used_distinct: u32,
    is_new_hash: bool,
    new_study_unlocked: bool,
) -> anyhow::Result<()> {
    if is_new_hash && !new_study_unlocked {
        anyhow::bail!(
            "refusing new config: remaining variant slots are reserved for a documented new OOS study. \
             Re-run a known hash, or pass --unlock-new-study and type NEW-OOS. \
             ({used_distinct}/{GATE_MAX_VARIANTS} distinct configs used)"
        );
    }
    if is_new_hash && used_distinct >= GATE_MAX_VARIANTS {
        anyhow::bail!(
            "refusing new config #{}: variant budget exhausted \
             ({used_distinct}/{GATE_MAX_VARIANTS} distinct configs). Per spec §5 the \
             budget never resets — new research requires a fresh out-of-sample \
             window, documented before running.",
            used_distinct + 1
        );
    }
    Ok(())
}
```

`run_search`: change signature to `async fn run_search(config_path: &str, unlock_new_study: bool)`. After opening db / loading used+known, before the variant loop:

```rust
    if unlock_new_study {
        confirm_new_study()?;
    }
```

Inside the loop, **before** `evaluate_config`:

```rust
        check_variant_budget(used, !known.contains(&hash), unlock_new_study)?;
```

Do not add a `config_state` lock key. Delete the old two-arg `variant_budget_refuses_21st_distinct_hash_but_reruns_are_free` test (replaced in Step 1).

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test variant_budget new_study_unlock search_subcommand_parses_unlock_flag
cargo test
```

Expected: all green, including previous Task 1 tests.

- [ ] **Step 5: Commit**

```bash
git add src/main.rs
git commit -m "Task 2: lock new search hashes unless NEW-OOS unlock"
```

---

### Task 3: README, RUNBOOK, smoke_serious

**Files:**
- Modify: `README.md` (status + commands table + live sentence)
- Modify: `docs/RUNBOOK.md` §0
- Modify: `scripts/smoke_serious.sh` section 6 live checks

**Interfaces:**
- Consumes: Task 1 refuse substring `live trading refused until a stored overall gate PASS exists`; Task 2 lock is already covered by `cargo test` (smoke_serious does not run `search` with a new hash)
- Produces: operator-facing copy that live is blocked in code; smoke still exits 0 on this repo’s FAIL-on-disk DB

- [ ] **Step 1: Update README status and live sentence**

Replace the current gate/live blurb (lines 6–8) with:

```markdown
**Current gate verdict: NO-GO — research only.** Live is refused in code until a
stored overall PASS. 8 of 20 lifetime variant slots are reserved; `search`
replays the known 12 for free and will not spend new hashes unless
`--unlock-new-study` + typed `NEW-OOS` (new OOS window, documented first).
Testnet-first: `trade` defaults to `https://testnet.binance.vision`.
```

In the commands table, change the `search` and `trade` rows to:

```markdown
| `search [--unlock-new-study]` | replay the known grid; new hashes locked |
| `trade [--once] [--dry-run] [--testnet\|--live]` | executor loop (testnet default; `--live` requires stored PASS then `GO`) |
```

- [ ] **Step 2: Update RUNBOOK §0** to say live is **blocked in code**, not merely advised against:

```markdown
## 0. Status warning

The pre-registered gate verdict is **NO-GO**. `trade --live` exits non-zero
until a stored overall PASS exists (`GO` cannot override). `flatten --live`
remains the kill switch (still requires typed `GO`). Testnet is the sanctioned
engine stage. Variant budget: 12/20 used; remaining 8 are reserved — do not
run `search --unlock-new-study` on this OOS window.
```

- [ ] **Step 3: Update `scripts/smoke_serious.sh` live checks** so they match Task 1 (FAIL on disk, no stdin `GO` path):

Replace:

```bash
OUT=$(printf 'no\n' | env BINANCE_API_KEY=fake BINANCE_API_SECRET=fake $BIN trade --once --live 2>&1)
echo "$OUT" | grep -qi "NO-GO"; check $? "live gate banner shows NO-GO"
echo "$OUT" | grep -qi "abort\|refus\|cancel"; check $? "non-GO answer aborts live mode"
```

with:

```bash
OUT=$(env BINANCE_API_KEY=fake BINANCE_API_SECRET=fake $BIN trade --once --live 2>&1)
echo "$OUT" | grep -qi "NO-GO"; check $? "live gate banner shows NO-GO"
echo "$OUT" | grep -q "live trading refused until a stored overall gate PASS exists"
check $? "live refused in code without GO prompt"
```

Do not pipe `printf 'no\n'` — refuse happens before stdin. Keep the keyless `trade` and `flatten` checks.

- [ ] **Step 4: Run unit tests (smoke_serious hits live Binance `fetch`; skip unless the operator already uses it)**

```bash
cargo test
```

Expected: green. Optionally `scripts/smoke_local.sh` (no network keys, no `--live`).

- [ ] **Step 5: Commit**

```bash
git add README.md docs/RUNBOOK.md scripts/smoke_serious.sh
git commit -m "Task 3: document research-only freeze and update smoke live check"
```

---

## Spec coverage (self-review)

| Spec section | Task |
|---|---|
| §4 live hard-refuse, including `--live --dry-run`, flatten exception, testnet unchanged | Task 1 |
| §5 lock, unlock phrase, cap 20 after unlock, no sqlite lock bit, abort on first new hash | Task 2 |
| §6 unlock is not a new study | docs in Task 3 (README/RUNBOOK); no extra code |
| §7 README + RUNBOOK; `.env.example` unchanged | Task 3 |
| §8 test list | Task 1 + Task 2 tests |
| §9 non-goals | no tasks for Polymarket / WS / launchd / resetting budget |
| §10 anyhow errors, no secrets, no partial `backtest_runs` | Task 1 messages + Task 2 refuse-before-evaluate |
| §11 tests first, then guards, then docs | task order 1 → 2 → 3 |

## Placeholder scan

No TBD/TODO. Function names are `live_trade_permitted`, `refuse_live_trade`, `confirm_new_study_input`, `confirm_new_study`, `check_variant_budget(..., new_study_unlocked)`.

#!/bin/bash
# Local smoke suite (plan Task 7): build + full test suite + CLI refusals.
# Exits non-zero on ANY failure. Never touches repo data/ or .env — all
# runtime steps execute from a throwaway temp working directory.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CONFIG="$ROOT/config/strategy_ema_rsi.toml"

fail() {
  echo "smoke: FAIL: $*" >&2
  exit 1
}

# honor .cargo/config.toml build.target-dir (this repo redirects off-volume)
TARGET_DIR="$(cd "$ROOT" && cargo metadata --no-deps --format-version 1 \
  | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')"
TARGET_DIR="${TARGET_DIR:-$ROOT/target}"
BIN="$TARGET_DIR/debug/trading_platform"

echo "== smoke 1/4: cargo build =="
(cd "$ROOT" && cargo build) || fail "cargo build"

echo "== smoke 2/4: cargo test =="
(cd "$ROOT" && cargo test) >"$TARGET_DIR/smoke_test.log" 2>&1 \
  || { tail -20 "$TARGET_DIR/smoke_test.log" >&2; fail "cargo test"; }
grep -q "test result: ok" "$TARGET_DIR/smoke_test.log" || fail "no green test result line"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

echo "== smoke 3/4: trade refuses cleanly without keys =="
set +e
(
  cd "$TMP" &&
    env -u BINANCE_API_KEY -u BINANCE_API_SECRET "$BIN" trade \
      --config "$CONFIG" --once --dry-run --testnet
) >"$TMP/trade.log" 2>&1
rc=$?
set -e
[ "$rc" -ne 0 ] || fail "trade without keys exited 0 — expected clean refusal"
grep -q "missing env var BINANCE_API_KEY" "$TMP/trade.log" ||
  fail "refusal log line missing (want 'missing env var BINANCE_API_KEY'); got: $(cat "$TMP/trade.log")"

echo "== smoke 4/4: export writes CSV =="
(
  cd "$TMP" &&
    "$BIN" export --out "$TMP/trades.csv"
) >"$TMP/export.log" 2>&1 || { cat "$TMP/export.log" >&2; fail "export exited non-zero"; }
[ -f "$TMP/trades.csv" ] || fail "export produced no CSV"
head -1 "$TMP/trades.csv" | grep -q "^id,client_order_id,symbol,side,qty,entry_price,entry_ts," ||
  fail "CSV header unexpected: $(head -1 "$TMP/trades.csv")"

echo "smoke: OK (build + tests + keyless refusal + export)"

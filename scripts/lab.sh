#!/bin/bash
# Lab tech: refresh klines + replay the current config's backtest.
# Never search, never trade, never flatten. No LLM. No new variant hashes.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CONFIG="$ROOT/config/strategy_ema_rsi.toml"

TARGET_DIR="$(cd "$ROOT" && cargo metadata --no-deps --format-version 1 \
  | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')"
TARGET_DIR="${TARGET_DIR:-$ROOT/target}"
BIN="$TARGET_DIR/release/trading_platform"
if [ ! -x "$BIN" ]; then
  BIN="$TARGET_DIR/debug/trading_platform"
fi
if [ ! -x "$BIN" ]; then
  echo "lab: building debug binary" >&2
  (cd "$ROOT" && cargo build)
  BIN="$TARGET_DIR/debug/trading_platform"
fi

echo "lab: research-only — fetch + backtest. no search, no trade, no live."
(cd "$ROOT" && "$BIN" fetch --config "$CONFIG")
(cd "$ROOT" && "$BIN" backtest --config "$CONFIG")
echo "lab: done. this is measurement, not a go-live signal."

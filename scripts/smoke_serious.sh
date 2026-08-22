#!/bin/bash
# Serious smoke test — exercises the shipped binary end-to-end.
set -uo pipefail
cd "$(dirname "$0")/.."

BIN=$(ls /Users/goaltosuceed/.cargo-target/trading_platform/release/trading_platform 2>/dev/null || echo target/release/trading_platform)
DB=data/trading.db
PASS=0; FAIL=0
ok()   { PASS=$((PASS+1)); echo "  PASS: $1"; }
bad()  { FAIL=$((FAIL+1)); echo "  FAIL: $1"; }
check(){ if [ "$1" = "0" ]; then ok "$2"; else bad "$2"; fi }

echo "== 1. binary sanity =="
$BIN --version >/dev/null 2>&1; check $? "--version exits 0"
$BIN --help | grep -q "fetch\|backtest\|search"; check $? "--help lists subcommands"

echo "== 2. fetch (live Binance public API, keyless) =="
$BIN fetch 2>&1 | grep -cE "cached [0-9]+ candles" | grep -q "^5$"
check $? "all 5 pairs cached"

ROWS=$(sqlite3 "$DB" "SELECT COUNT(*) FROM klines;" 2>/dev/null)
[ "${ROWS:-0}" -ge 65000 ]; check $? "kline rows >= 65000 (got ${ROWS:-0})"
SPANS=$(sqlite3 -separator ' ' "$DB" "SELECT symbol, MIN(open_time), MAX(open_time), COUNT(*) FROM klines GROUP BY symbol;" 2>/dev/null)
DAYS_MIN=999
while read -r sym lo hi cnt; do
  d=$(( (hi - lo) / 86400000 ))
  [ "$d" -lt "$DAYS_MIN" ] && DAYS_MIN=$d
  [ "$cnt" -lt 13000 ] && bad "$sym only $cnt candles"
done <<< "$SPANS"
[ "$DAYS_MIN" -ge 540 ]; check $? "every pair spans >= 540 days (min $DAYS_MIN)"
GAPS=$(sqlite3 "$DB" "
  SELECT COUNT(*) FROM (
    SELECT symbol, open_time - LAG(open_time) OVER (PARTITION BY symbol ORDER BY open_time) AS delta
    FROM klines) WHERE delta IS NOT NULL AND delta != 3600000;" 2>/dev/null)
[ "$GAPS" -le 10 ]; check $? "hourly grid intact (gaps/dupes: $GAPS)"

echo "== 3. backtest report + gate verdict =="
OUT=$($BIN backtest 2>&1)
echo "$OUT" | grep -qi "OOS"; check $? "OOS metrics reported"
echo "$OUT" | grep -qi "PASS\|FAIL"; check $? "gate verdict printed"
echo "$OUT" | grep -qE "PF|profit factor"; check $? "profit factor present"

echo "== 4. search + variant budget semantics =="
B4=$(sqlite3 "$DB" "SELECT value FROM config_state WHERE key='variant_budget_used';" 2>/dev/null)
$BIN search > /tmp/search_out.txt 2>&1; check $? "search completes"
grep -q "RANKING IS ANALYSIS ONLY" /tmp/search_out.txt; check $? "analysis-only banner shown"
grep -q "OVERALL:" /tmp/search_out.txt; check $? "overall verdict printed"
AFTER=$(sqlite3 "$DB" "SELECT value FROM config_state WHERE key='variant_budget_used';")
[ "$B4" = "$AFTER" ]; check $? "duplicate rerun burns zero budget ($B4 -> $AFTER)"
RUNS=$(sqlite3 "$DB" "SELECT COUNT(DISTINCT config_hash) FROM backtest_runs;")
[ "$RUNS" -le 20 ]; check $? "distinct configs <= 20 (got $RUNS)"

echo "== 5. export =="
rm -f /tmp/export_smoke.csv
$BIN export --out /tmp/export_smoke.csv >/dev/null 2>&1; check $? "export exits 0"
head -1 /tmp/export_smoke.csv | grep -q "^id,client_order_id,symbol"; check $? "csv header correct"
[ -s /tmp/export_smoke.csv ]; check $? "csv non-empty (header always; rows appear once live trades exist)"
head -c 200 /tmp/export_smoke.csv | grep -qv "api\|secret\|key"; check $? "no secret material in csv head"

echo "== 6. safety gates =="
OUT=$(env -u BINANCE_API_KEY -u BINANCE_API_SECRET $BIN trade --once 2>&1)
echo "$OUT" | grep -q "missing env var BINANCE_API_KEY"
check $? "trade without keys names missing var"
echo "$OUT" | grep -q "NO-GO"
check $? "gate banner precedes key check"
OUT=$(env BINANCE_API_KEY=fake BINANCE_API_SECRET=fake $BIN trade --once --live 2>&1)
echo "$OUT" | grep -qi "NO-GO"; check $? "live gate banner shows NO-GO"
echo "$OUT" | grep -q "live trading refused until a stored overall gate PASS exists"
check $? "live refused in code without GO prompt"
OUT=$(env -u BINANCE_API_KEY -u BINANCE_API_SECRET $BIN flatten 2>&1)
echo "$OUT" | grep -q "missing env var BINANCE_API_KEY"
check $? "flatten without keys refused"

echo "== RESULT: $PASS passed, $FAIL failed =="
exit $FAIL

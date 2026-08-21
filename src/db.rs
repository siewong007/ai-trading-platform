use crate::types::Candle;
use sqlx::{sqlite::SqlitePoolOptions, Row, SqlitePool};

#[derive(Clone)]
pub struct Db {
    pool: SqlitePool,
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS klines(
  symbol TEXT NOT NULL,
  interval TEXT NOT NULL,
  open_time INTEGER NOT NULL,
  open REAL NOT NULL,
  high REAL NOT NULL,
  low REAL NOT NULL,
  close REAL NOT NULL,
  volume REAL NOT NULL,
  PRIMARY KEY(symbol, interval, open_time)
);
CREATE TABLE IF NOT EXISTS trades(
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  client_order_id TEXT UNIQUE,
  symbol TEXT NOT NULL,
  side TEXT NOT NULL,
  qty REAL NOT NULL,
  entry_price REAL NOT NULL,
  entry_ts INTEGER NOT NULL,
  exit_price REAL,
  exit_ts INTEGER,
  fee_paid REAL NOT NULL DEFAULT 0,
  pnl REAL,
  pnl_pct REAL,
  strategy TEXT NOT NULL,
  mode TEXT NOT NULL CHECK(mode IN ('backtest','shadow','live'))
);
CREATE TABLE IF NOT EXISTS orders(
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  client_order_id TEXT UNIQUE NOT NULL,
  symbol TEXT NOT NULL,
  side TEXT NOT NULL,
  otype TEXT NOT NULL,
  price REAL,
  qty REAL NOT NULL,
  status TEXT NOT NULL,
  exchange_id TEXT,
  created_ts INTEGER NOT NULL,
  updated_ts INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS positions(
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  symbol TEXT NOT NULL,
  qty REAL NOT NULL,
  entry_price REAL NOT NULL,
  stop_price REAL NOT NULL,
  target_price REAL NOT NULL,
  opened_ts INTEGER NOT NULL,
  closed_ts INTEGER,
  status TEXT NOT NULL CHECK(status IN ('open','closed','cancelled'))
);
CREATE TABLE IF NOT EXISTS equity_curve(
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  ts INTEGER NOT NULL,
  equity REAL NOT NULL,
  drawdown REAL NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS signals_log(
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  ts INTEGER NOT NULL,
  symbol TEXT NOT NULL,
  strategy TEXT NOT NULL,
  details TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS config_state(
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS backtest_runs(
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  config_hash TEXT NOT NULL,
  symbol TEXT NOT NULL,
  rsi_entry REAL NOT NULL,
  atr_mult REAL NOT NULL,
  rr REAL NOT NULL,
  oos_trades INTEGER NOT NULL,
  oos_pf REAL NOT NULL,
  oos_pnl REAL NOT NULL,
  oos_dd REAL NOT NULL,
  ran_at INTEGER NOT NULL
);
"#;

impl Db {
    pub async fn open(url: &str) -> anyhow::Result<Self> {
        let pool = SqlitePoolOptions::new()
            .max_connections(if url.contains(":memory:") { 1 } else { 4 })
            .connect(url)
            .await?;
        sqlx::query(SCHEMA).execute(&pool).await?;
        Ok(Self { pool })
    }

    pub async fn open_default() -> anyhow::Result<Self> {
        std::fs::create_dir_all("data")?;
        // mode=rwc: sqlx will not create the file otherwise (code 14 if absent)
        Self::open("sqlite://data/trading.db?mode=rwc").await
    }

    pub async fn upsert_klines(
        &self,
        symbol: &str,
        interval: &str,
        candles: &[Candle],
    ) -> anyhow::Result<()> {
        let mut tx = self.pool.begin().await?;
        for c in candles {
            sqlx::query(
                "INSERT INTO klines(symbol,interval,open_time,open,high,low,close,volume)
                 VALUES(?,?,?,?,?,?,?,?)
                 ON CONFLICT(symbol,interval,open_time) DO UPDATE SET
                   open=excluded.open, high=excluded.high, low=excluded.low,
                   close=excluded.close, volume=excluded.volume",
            )
            .bind(symbol)
            .bind(interval)
            .bind(c.open_time)
            .bind(c.open)
            .bind(c.high)
            .bind(c.low)
            .bind(c.close)
            .bind(c.volume)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn load_klines(&self, symbol: &str, interval: &str) -> anyhow::Result<Vec<Candle>> {
        let rows = sqlx::query(
            "SELECT open_time,open,high,low,close,volume FROM klines
             WHERE symbol=? AND interval=? ORDER BY open_time ASC",
        )
        .bind(symbol)
        .bind(interval)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| Candle {
                open_time: r.get::<i64, _>(0),
                open: r.get::<f64, _>(1),
                high: r.get::<f64, _>(2),
                low: r.get::<f64, _>(3),
                close: r.get::<f64, _>(4),
                volume: r.get::<f64, _>(5),
            })
            .collect())
    }

    pub async fn config_get(&self, key: &str) -> anyhow::Result<Option<String>> {
        let mut conn = self.pool.acquire().await?;
        Self::config_get_in(&mut conn, key).await
    }

    async fn config_get_in(
        conn: &mut sqlx::SqliteConnection,
        key: &str,
    ) -> anyhow::Result<Option<String>> {
        let row = sqlx::query("SELECT value FROM config_state WHERE key=?")
            .bind(key)
            .fetch_optional(conn)
            .await?;
        Ok(row.map(|r| r.get::<String, _>(0)))
    }

    async fn config_set_in(
        conn: &mut sqlx::SqliteConnection,
        key: &str,
        value: &str,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO config_state(key,value) VALUES(?,?)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        )
        .bind(key)
        .bind(value)
        .execute(conn)
        .await?;
        Ok(())
    }

    /// Config hashes already recorded in backtest_runs — re-running any of
    /// these is free under the variant budget.
    pub async fn known_config_hashes(&self) -> anyhow::Result<std::collections::HashSet<String>> {
        let rows = sqlx::query("SELECT DISTINCT config_hash FROM backtest_runs")
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.into_iter().map(|r| r.get::<String, _>(0)).collect())
    }

    /// Number of DISTINCT config hashes ever recorded.
    pub async fn distinct_config_count(&self) -> anyhow::Result<u32> {
        let row = sqlx::query("SELECT COUNT(DISTINCT config_hash) AS n FROM backtest_runs")
            .fetch_one(&self.pool)
            .await?;
        Ok(row.get::<i64, _>("n") as u32)
    }
}

/// One OOS result row recorded for a backtested config on one symbol.
#[derive(Debug)]
pub struct BacktestRunRow {
    pub symbol: String,
    pub rsi_entry: f64,
    pub atr_mult: f64,
    pub rr: f64,
    pub oos_trades: i64,
    pub oos_pf: f64,
    pub oos_pnl: f64,
    pub oos_dd: f64,
}

impl Db {
    /// Persist OOS results for one config hash with UPSERT semantics (stale
    /// rows for the same hash+symbol are replaced, never duplicated), and
    /// charge the variant-budget counter exactly ONCE per DISTINCT hash —
    /// inside the SAME transaction as the inserts, so a mid-grid crash cannot
    /// lose the charge.
    pub async fn record_backtest_results(
        &self,
        config_hash: &str,
        rows: &[BacktestRunRow],
    ) -> anyhow::Result<()> {
        let mut tx = self.pool.begin().await?;
        // existence check BEFORE deleting: decides whether this hash is new
        let existed = sqlx::query("SELECT 1 FROM backtest_runs WHERE config_hash=? LIMIT 1")
            .bind(config_hash)
            .fetch_optional(&mut *tx)
            .await?
            .is_some();
        if !existed {
            let used: u32 = Self::config_get_in(&mut tx, "variant_budget_used")
                .await?
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            Self::config_set_in(&mut tx, "variant_budget_used", &(used + 1).to_string()).await?;
        }
        for r in rows {
            sqlx::query("DELETE FROM backtest_runs WHERE config_hash=? AND symbol=?")
                .bind(config_hash)
                .bind(&r.symbol)
                .execute(&mut *tx)
                .await?;
            sqlx::query(
                "INSERT INTO backtest_runs(config_hash,symbol,rsi_entry,atr_mult,rr,
                 oos_trades,oos_pf,oos_pnl,oos_dd,ran_at)
                 VALUES(?,?,?,?,?,?,?,?,?,?)",
            )
            .bind(config_hash)
            .bind(&r.symbol)
            .bind(r.rsi_entry)
            .bind(r.atr_mult)
            .bind(r.rr)
            .bind(r.oos_trades)
            .bind(r.oos_pf)
            .bind(r.oos_pnl)
            .bind(r.oos_dd)
            .bind(chrono::Utc::now().timestamp_millis())
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn mem_db() -> Db {
        Db::open("sqlite::memory:").await.unwrap()
    }

    #[tokio::test]
    async fn file_db_creates_and_roundtrips() {
        let path = std::env::temp_dir().join(format!("tp_probe_{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        // file does not exist yet — open must create it (mode=rwc)
        let db = Db::open(&format!("sqlite://{}?mode=rwc", path.display()))
            .await
            .unwrap();
        let c = Candle {
            open_time: 1,
            open: 1.0,
            high: 2.0,
            low: 0.5,
            close: 1.5,
            volume: 3.0,
        };
        db.upsert_klines("TESTUSDT", "1h", &[c]).await.unwrap();
        assert!(path.exists());
        let loaded = db.load_klines("TESTUSDT", "1h").await.unwrap();
        assert_eq!(loaded.len(), 1);
        assert!((loaded[0].close - 1.5).abs() < 1e-9);
        drop(db);
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn kline_roundtrip_preserves_order() {
        let db = mem_db().await;
        let mk = |t: i64, p: f64| Candle {
            open_time: t,
            open: p,
            high: p + 1.0,
            low: p - 1.0,
            close: p,
            volume: 10.0,
        };
        let candles = vec![mk(3000, 103.0), mk(1000, 101.0), mk(2000, 102.0)];
        db.upsert_klines("BTCUSDT", "1h", &candles).await.unwrap();
        let loaded = db.load_klines("BTCUSDT", "1h").await.unwrap();
        assert_eq!(loaded.len(), 3);
        assert_eq!(loaded[0].open_time, 1000); // sorted regardless of insert order
        assert!((loaded[2].close - 103.0).abs() < 1e-9);

        // upsert overwrites same key
        db.upsert_klines("BTCUSDT", "1h", &[mk(1000, 999.0)])
            .await
            .unwrap();
        let loaded = db.load_klines("BTCUSDT", "1h").await.unwrap();
        assert_eq!(loaded.len(), 3);
        assert!((loaded[0].close - 999.0).abs() < 1e-9);
    }

    #[tokio::test]
    async fn config_state_roundtrip() {
        let db = mem_db().await;
        assert_eq!(db.config_get("variants").await.unwrap(), None);
        {
            // scoped: :memory: pool has ONE connection — release it before
            // config_get acquires again
            let mut conn = db.pool.acquire().await.unwrap();
            Db::config_set_in(&mut conn, "variants", "1").await.unwrap();
            Db::config_set_in(&mut conn, "variants", "2").await.unwrap();
        }
        assert_eq!(
            db.config_get("variants").await.unwrap(),
            Some("2".to_string())
        );
    }

    fn br_row(symbol: &str, pnl: f64) -> BacktestRunRow {
        BacktestRunRow {
            symbol: symbol.into(),
            rsi_entry: 35.0,
            atr_mult: 2.0,
            rr: 1.5,
            oos_trades: 25,
            oos_pf: 1.4,
            oos_pnl: pnl,
            oos_dd: 5.0,
        }
    }

    async fn scalar(db: &Db, sql: &str) -> i64 {
        sqlx::query(sql)
            .fetch_one(&db.pool)
            .await
            .unwrap()
            .get::<i64, _>(0)
    }

    #[tokio::test]
    async fn rerunning_a_known_hash_is_free_and_upserts() {
        let db = mem_db().await;
        db.record_backtest_results("h1", &[br_row("BTCUSDT", 1.0)])
            .await
            .unwrap();
        // duplicate invocation of the SAME hash: new numbers replace the row —
        // no duplicate row, no extra budget charge
        db.record_backtest_results("h1", &[br_row("BTCUSDT", 2.0)])
            .await
            .unwrap();
        assert_eq!(
            scalar(
                &db,
                "SELECT COUNT(*) FROM backtest_runs WHERE config_hash='h1'"
            )
            .await,
            1
        );
        let pnl = sqlx::query("SELECT oos_pnl FROM backtest_runs WHERE config_hash='h1'")
            .fetch_one(&db.pool)
            .await
            .unwrap()
            .get::<f64, _>(0);
        assert!((pnl - 2.0).abs() < 1e-9);
        assert_eq!(
            db.config_get("variant_budget_used")
                .await
                .unwrap()
                .as_deref(),
            Some("1")
        );
    }

    #[tokio::test]
    async fn budget_charged_once_per_distinct_hash() {
        let db = mem_db().await;
        db.record_backtest_results("h1", &[br_row("A", 1.0)])
            .await
            .unwrap();
        db.record_backtest_results("h2", &[br_row("A", 2.0)])
            .await
            .unwrap();
        db.record_backtest_results("h1", &[br_row("B", 3.0)])
            .await
            .unwrap(); // free rerun
        assert_eq!(
            db.config_get("variant_budget_used")
                .await
                .unwrap()
                .as_deref(),
            Some("2")
        );
        assert_eq!(db.distinct_config_count().await.unwrap(), 2);
        assert_eq!(db.known_config_hashes().await.unwrap().len(), 2);
        // h1 now has rows for both symbols (A from first run, B from rerun)
        assert_eq!(
            scalar(
                &db,
                "SELECT COUNT(*) FROM backtest_runs WHERE config_hash='h1'"
            )
            .await,
            2
        );
    }
}

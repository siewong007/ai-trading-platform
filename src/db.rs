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
        Self::open("sqlite://data/trading.db").await
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
        let row = sqlx::query("SELECT value FROM config_state WHERE key=?")
            .bind(key)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| r.get::<String, _>(0)))
    }

    pub async fn config_set(&self, key: &str, value: &str) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO config_state(key,value) VALUES(?,?)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        )
        .bind(key)
        .bind(value)
        .execute(&self.pool)
        .await?;
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
        db.config_set("variants", "1").await.unwrap();
        db.config_set("variants", "2").await.unwrap();
        assert_eq!(
            db.config_get("variants").await.unwrap(),
            Some("2".to_string())
        );
    }
}

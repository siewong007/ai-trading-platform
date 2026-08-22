use std::collections::HashMap;
use std::sync::Mutex;
use tokio::sync::mpsc;

/// Per-subscriber channel capacity. A slow receiver whose buffer is full has
/// NEW messages dropped (oldest preserved); delivery never blocks the publisher.
const CHANNEL_CAPACITY: usize = 64;

/// Synchronous typed-topic pub/sub bus. Topics are plain strings
/// (`kline.BTCUSDT` style); payloads are raw strings. Subscribers receive
/// messages over tokio mpsc channels matched by exact topic or `prefix.*`.
pub struct Bus {
    inner: Mutex<HashMap<String, Vec<mpsc::Sender<String>>>>,
}

impl Default for Bus {
    fn default() -> Self {
        Self::new()
    }
}

impl Bus {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
        }
    }

    /// Subscribe to an exact topic or a `prefix.*` wildcard pattern.
    pub fn subscribe(&self, topic_pattern: &str) -> mpsc::Receiver<String> {
        let (tx, rx) = mpsc::channel(CHANNEL_CAPACITY);
        self.inner
            .lock()
            .unwrap()
            .entry(topic_pattern.to_string())
            .or_default()
            .push(tx);
        rx
    }

    /// Fan-out `payload` to every subscriber matching `topic`. Drops messages
    /// on closed or full channels (never blocks); prunes closed senders.
    pub fn publish(&self, topic: &str, payload: &str) {
        let mut map = self.inner.lock().unwrap();
        for (pattern, senders) in map.iter_mut() {
            if !matches(pattern, topic) {
                continue;
            }
            senders.retain(|tx| match tx.try_send(payload.to_string()) {
                Ok(()) => true,
                // Full channel: new message dropped, subscriber kept.
                Err(mpsc::error::TrySendError::Full(_)) => true,
                Err(mpsc::error::TrySendError::Closed(_)) => false,
            });
        }
        map.retain(|_, senders| !senders.is_empty());
    }

    /// Live subscriber count across patterns matching `topic`.
    pub fn subscriber_count(&self, topic: &str) -> usize {
        let mut map = self.inner.lock().unwrap();
        let mut count = 0;
        for (pattern, senders) in map.iter_mut() {
            if !matches(pattern, topic) {
                continue;
            }
            senders.retain(|tx| !tx.is_closed());
            count += senders.len();
        }
        map.retain(|_, senders| !senders.is_empty());
        count
    }
}

/// `prefix.*` matches any topic starting with `prefix`; otherwise exact match.
fn matches(pattern: &str, topic: &str) -> bool {
    match pattern.strip_suffix(".*") {
        Some(prefix) => topic.starts_with(prefix),
        None => pattern == topic,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delivers_to_exact_topic_subscriber() {
        let bus = Bus::new();
        let mut rx = bus.subscribe("kline.BTCUSDT");
        bus.publish("kline.BTCUSDT", r#"{"c":50000}"#);
        assert_eq!(rx.try_recv().unwrap(), r#"{"c":50000}"#);
    }

    #[test]
    fn delivers_through_prefix_wildcard_only() {
        let bus = Bus::new();
        let mut rx = bus.subscribe("kline.*");
        bus.publish("trade.BTCUSDT", "missed");
        bus.publish("kline.BTCUSDT", "hit");
        assert_eq!(rx.try_recv().unwrap(), "hit");
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn publish_without_subscribers_is_silent_drop() {
        let bus = Bus::new();
        bus.publish("kline.BTCUSDT", r#"{"c":1}"#);
        assert_eq!(bus.subscriber_count("kline.BTCUSDT"), 0);
    }

    #[test]
    fn closed_receiver_is_pruned_and_count_shrinks() {
        let bus = Bus::new();
        let rx = bus.subscribe("kline.BTCUSDT");
        assert_eq!(bus.subscriber_count("kline.BTCUSDT"), 1);
        drop(rx);
        assert_eq!(bus.subscriber_count("kline.BTCUSDT"), 0);
        bus.publish("kline.BTCUSDT", "after-drop");
        assert_eq!(bus.subscriber_count("kline.BTCUSDT"), 0);
    }

    #[test]
    fn full_channel_drops_new_message_and_keeps_oldest() {
        let bus = Bus::new();
        let mut rx = bus.subscribe("kline.BTCUSDT");
        for i in 0..CHANNEL_CAPACITY + 6 {
            bus.publish("kline.BTCUSDT", &format!("m{i}"));
        }
        // Oldest CHANNEL_CAPACITY messages delivered in order; newer dropped.
        for i in 0..CHANNEL_CAPACITY {
            assert_eq!(rx.try_recv().unwrap(), format!("m{i}"));
        }
        assert!(rx.try_recv().is_err());
        // Channel drained: publishing works again.
        bus.publish("kline.BTCUSDT", "fresh");
        assert_eq!(rx.try_recv().unwrap(), "fresh");
    }

    #[test]
    fn fan_out_reaches_exact_and_wildcard_subscribers() {
        let bus = Bus::new();
        let mut exact = bus.subscribe("kline.BTCUSDT");
        let mut wild = bus.subscribe("kline.*");
        bus.publish("kline.BTCUSDT", "both");
        assert_eq!(exact.try_recv().unwrap(), "both");
        assert_eq!(wild.try_recv().unwrap(), "both");
        assert_eq!(bus.subscriber_count("kline.BTCUSDT"), 2);
    }
}

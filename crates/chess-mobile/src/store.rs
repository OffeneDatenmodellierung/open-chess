//! What the app keeps on the device, in SQLite: saved games, the outbox of
//! things waiting to be sent (finished games, confirmations, board
//! corrections), each speed's rating and the settings.
//!
//! The outbox is keyed by the app's own id for each item, which the
//! server also uses to recognise a repeated upload, so sending twice is
//! harmless. Low-data mode asks for games only and a byte budget.

use std::sync::{Mutex, MutexGuard};

use rusqlite::{Connection, OptionalExtension, params};

use crate::MobileError;
use crate::ratings::RatingValue;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS games (
    id TEXT PRIMARY KEY,
    record TEXT NOT NULL,
    updated_ms INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS outbox (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    key TEXT NOT NULL UNIQUE,
    kind TEXT NOT NULL,
    payload BLOB NOT NULL,
    created_ms INTEGER NOT NULL,
    sent_ms INTEGER
);
CREATE TABLE IF NOT EXISTS ratings (
    speed TEXT PRIMARY KEY,
    rating REAL NOT NULL,
    deviation REAL NOT NULL,
    volatility REAL NOT NULL,
    local INTEGER NOT NULL,
    synced_ms INTEGER
);
CREATE TABLE IF NOT EXISTS settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
PRAGMA user_version = 1;
";

/// Kinds of outbox item. Photos are the big ones low-data mode holds back.
pub const KIND_PHOTO: &str = "photo";

/// Something waiting to be sent.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct OutboxItem {
    /// The app's id for it, sent along so the server can ignore repeats.
    pub key: String,
    /// "game", "confirm", "photo"…
    pub kind: String,
    pub payload: Vec<u8>,
    pub created_ms: i64,
}

/// What is waiting to be sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct PendingTotals {
    pub items: u64,
    pub bytes: u64,
}

/// One speed's rating, and whether it is the device's provisional figure
/// (from offline games not yet confirmed) or the server's.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct SpeedRating {
    pub speed: String,
    pub value: RatingValue,
    pub local: bool,
}

#[derive(uniffi::Object)]
pub struct Store {
    db: Mutex<Connection>,
}

fn i64_of(n: u64) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

#[uniffi::export]
impl Store {
    /// Open (or create) the store at `path`, in the app's container.
    #[uniffi::constructor]
    pub fn open(path: String) -> Result<Self, MobileError> {
        Self::with(Connection::open(path)?)
    }

    /// A store that lives only as long as this object (previews, tests).
    #[uniffi::constructor]
    pub fn in_memory() -> Result<Self, MobileError> {
        Self::with(Connection::open_in_memory()?)
    }

    /// Save a game the app wants to come back to (its own JSON).
    pub fn save_game(&self, id: String, record: String, now_ms: u64) -> Result<(), MobileError> {
        self.lock().execute(
            "INSERT INTO games (id, record, updated_ms) VALUES (?1, ?2, ?3)
             ON CONFLICT (id) DO UPDATE SET record = excluded.record, updated_ms = excluded.updated_ms",
            params![id, record, i64_of(now_ms)],
        )?;
        Ok(())
    }

    pub fn load_game(&self, id: String) -> Result<Option<String>, MobileError> {
        Ok(self.lock().query_row("SELECT record FROM games WHERE id = ?1", [id], |r| r.get(0)).optional()?)
    }

    /// Saved games' ids, most recently changed first.
    pub fn game_ids(&self, limit: u32) -> Result<Vec<String>, MobileError> {
        let db = self.lock();
        let mut q = db.prepare("SELECT id FROM games ORDER BY updated_ms DESC LIMIT ?1")?;
        let ids = q.query_map([limit], |r| r.get(0))?.collect::<Result<_, _>>()?;
        Ok(ids)
    }

    pub fn delete_game(&self, id: String) -> Result<(), MobileError> {
        self.lock().execute("DELETE FROM games WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Queue something to send. Returns false when `key` is already queued
    /// (or was sent), so a retry doesn't queue it twice.
    pub fn enqueue(
        &self,
        key: String,
        kind: String,
        payload: Vec<u8>,
        now_ms: u64,
    ) -> Result<bool, MobileError> {
        let n = self.lock().execute(
            "INSERT OR IGNORE INTO outbox (key, kind, payload, created_ms) VALUES (?1, ?2, ?3, ?4)",
            params![key, kind, payload, i64_of(now_ms)],
        )?;
        Ok(n == 1)
    }

    /// What to send now, oldest first. Without `include_photos` (low-data
    /// mode, or not on Wi-Fi) photos wait. `max_bytes` caps the payloads'
    /// total (0: no cap); an item too big for what's left waits for the
    /// next sync rather than blocking smaller ones behind it.
    pub fn pending(&self, include_photos: bool, max_bytes: u64) -> Result<Vec<OutboxItem>, MobileError> {
        let db = self.lock();
        let mut q = db.prepare(
            "SELECT key, kind, payload, created_ms FROM outbox
             WHERE sent_ms IS NULL AND (?1 OR kind <> ?2) ORDER BY seq",
        )?;
        let rows = q.query_map(params![include_photos, KIND_PHOTO], |r| {
            Ok(OutboxItem { key: r.get(0)?, kind: r.get(1)?, payload: r.get(2)?, created_ms: r.get(3)? })
        })?;
        let mut out = Vec::new();
        let mut left = if max_bytes == 0 { u64::MAX } else { max_bytes };
        for item in rows {
            let item = item?;
            let size = item.payload.len() as u64;
            if size <= left {
                left -= size;
                out.push(item);
            }
        }
        Ok(out)
    }

    /// How many items, and how many bytes, are waiting (for the Sync screen).
    pub fn pending_totals(&self, include_photos: bool) -> Result<PendingTotals, MobileError> {
        let (n, bytes): (i64, i64) = self.lock().query_row(
            "SELECT count(*), coalesce(sum(length(payload)), 0) FROM outbox
             WHERE sent_ms IS NULL AND (?1 OR kind <> ?2)",
            params![include_photos, KIND_PHOTO],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok(PendingTotals { items: u64::try_from(n).unwrap_or(0), bytes: u64::try_from(bytes).unwrap_or(0) })
    }

    /// The server took these.
    pub fn mark_sent(&self, keys: Vec<String>, now_ms: u64) -> Result<(), MobileError> {
        let mut db = self.lock();
        let tx = db.transaction()?;
        for k in keys {
            tx.execute("UPDATE outbox SET sent_ms = ?2 WHERE key = ?1", params![k, i64_of(now_ms)])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Forget payloads sent before `before_ms`, keeping their keys so a
    /// late retry is still recognised.
    pub fn prune_sent(&self, before_ms: u64) -> Result<(), MobileError> {
        self.lock().execute(
            "UPDATE outbox SET payload = x'' WHERE sent_ms IS NOT NULL AND sent_ms < ?1",
            [i64_of(before_ms)],
        )?;
        Ok(())
    }

    /// The server's rating for a speed: replaces any provisional figure.
    pub fn set_server_rating(
        &self,
        speed: String,
        value: RatingValue,
        now_ms: u64,
    ) -> Result<(), MobileError> {
        self.put_rating(&speed, value, false, Some(i64_of(now_ms)))
    }

    /// A provisional rating worked out on the device after an offline game.
    pub fn set_local_rating(&self, speed: String, value: RatingValue) -> Result<(), MobileError> {
        self.put_rating(&speed, value, true, None)
    }

    pub fn rating(&self, speed: String) -> Result<Option<SpeedRating>, MobileError> {
        Ok(self
            .lock()
            .query_row(
                "SELECT speed, rating, deviation, volatility, local FROM ratings WHERE speed = ?1",
                [speed],
                Self::speed_rating,
            )
            .optional()?)
    }

    pub fn ratings(&self) -> Result<Vec<SpeedRating>, MobileError> {
        let db = self.lock();
        let mut q =
            db.prepare("SELECT speed, rating, deviation, volatility, local FROM ratings ORDER BY speed")?;
        let all = q.query_map([], Self::speed_rating)?.collect::<Result<_, _>>()?;
        Ok(all)
    }

    pub fn setting(&self, key: String) -> Result<Option<String>, MobileError> {
        Ok(self
            .lock()
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| r.get(0))
            .optional()?)
    }

    pub fn set_setting(&self, key: String, value: String) -> Result<(), MobileError> {
        self.lock().execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }
}

impl Store {
    fn with(db: Connection) -> Result<Self, MobileError> {
        db.execute_batch(SCHEMA)?;
        Ok(Store { db: Mutex::new(db) })
    }

    fn lock(&self) -> MutexGuard<'_, Connection> {
        self.db.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn put_rating(
        &self,
        speed: &str,
        v: RatingValue,
        local: bool,
        synced: Option<i64>,
    ) -> Result<(), MobileError> {
        self.lock().execute(
            "INSERT INTO ratings (speed, rating, deviation, volatility, local, synced_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (speed) DO UPDATE SET rating = excluded.rating, deviation = excluded.deviation,
               volatility = excluded.volatility, local = excluded.local,
               synced_ms = coalesce(excluded.synced_ms, ratings.synced_ms)",
            params![speed, v.rating, v.deviation, v.volatility, local, synced],
        )?;
        Ok(())
    }

    fn speed_rating(r: &rusqlite::Row<'_>) -> rusqlite::Result<SpeedRating> {
        Ok(SpeedRating {
            speed: r.get(0)?,
            value: RatingValue { rating: r.get(1)?, deviation: r.get(2)?, volatility: r.get(3)? },
            local: r.get(4)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ratings::{Score, bot_rating, new_rating, rating_after};

    #[test]
    fn outbox_low_data_and_budget() {
        let s = Store::in_memory().unwrap();
        assert!(s.enqueue("g1".into(), "game".into(), vec![0; 400], 1).unwrap());
        assert!(!s.enqueue("g1".into(), "game".into(), vec![0; 400], 2).unwrap(), "queued once");
        s.enqueue("p1".into(), KIND_PHOTO.into(), vec![0; 50_000], 3).unwrap();
        s.enqueue("c1".into(), "confirm".into(), vec![0; 100], 4).unwrap();

        let keys = |v: Vec<OutboxItem>| v.into_iter().map(|i| i.key).collect::<Vec<_>>();
        assert_eq!(keys(s.pending(false, 0).unwrap()), ["g1", "c1"]);
        assert_eq!(
            keys(s.pending(true, 1_000).unwrap()),
            ["g1", "c1"],
            "the photo waits for a bigger budget"
        );
        assert_eq!(s.pending_totals(true).unwrap(), PendingTotals { items: 3, bytes: 50_500 });

        s.mark_sent(vec!["g1".into()], 10).unwrap();
        assert_eq!(keys(s.pending(true, 0).unwrap()), ["p1", "c1"]);
        s.prune_sent(11).unwrap();
        assert!(
            !s.enqueue("g1".into(), "game".into(), vec![1], 12).unwrap(),
            "still recognised after pruning"
        );
    }

    #[test]
    fn ratings_games_and_settings() {
        let s = Store::in_memory().unwrap();
        let after = rating_after(new_rating(), bot_rating(3), Score::Win);
        s.set_local_rating("rapid".into(), after).unwrap();
        let r = s.rating("rapid".into()).unwrap().unwrap();
        assert!(r.local);
        s.set_server_rating("rapid".into(), new_rating(), 99).unwrap();
        assert!(!s.rating("rapid".into()).unwrap().unwrap().local);
        assert_eq!(s.ratings().unwrap().len(), 1);

        s.save_game("a".into(), "{}".into(), 1).unwrap();
        s.save_game("b".into(), "{}".into(), 2).unwrap();
        s.save_game("a".into(), "{\"x\":1}".into(), 3).unwrap();
        assert_eq!(s.game_ids(10).unwrap(), ["a", "b"]);
        assert_eq!(s.load_game("a".into()).unwrap().as_deref(), Some("{\"x\":1}"));
        s.delete_game("b".into()).unwrap();
        assert_eq!(s.game_ids(10).unwrap(), ["a"]);

        s.set_setting("syncMinutes".into(), "10".into()).unwrap();
        assert_eq!(s.setting("syncMinutes".into()).unwrap().as_deref(), Some("10"));
        assert_eq!(s.setting("nope".into()).unwrap(), None);
    }
}

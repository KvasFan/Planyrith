//! Implements docs/11-persistence-and-web.md §2 (autosave and recovery) for
//! every frontend: a journal of the commands committed since the last
//! snapshot (`<doc>.plyr.journal`, JSON lines) and a periodic snapshot
//! (`<doc>.autosave.plyr`, the full native container). Recovery replays the
//! journal on the snapshot, or on the document itself when no snapshot was
//! taken yet. Autosave never writes the document's own key.
//!
//! The core has no clock: callers pass the time in seconds from any origin
//! (the GUI uses its frame clock) and the RFC 3339 stamps for the snapshot.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::commands::{CommandRegistry, Engine};
use crate::document::Document;
use crate::io::native::{HistoryEntry, LoadedDocument, read_document};
use crate::io::storage::{Storage, StorageKey};
use crate::io::{IoError, NoProgress};

/// A snapshot is taken after this many journaled commands (docs/11 §2).
pub const SNAPSHOT_EVERY_COMMANDS: usize = 200;

/// …or when this many seconds passed since the last one with commands
/// journaled in between (docs/11 §2).
pub const SNAPSHOT_EVERY_SECONDS: f64 = 120.0;

/// A journal row (docs/11 §2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JournalRow {
    /// Sequence number, from 1 after each snapshot.
    pub seq: u64,
    /// Command name.
    pub command: String,
    /// Params as executed.
    pub params: serde_json::Value,
    /// Undo group label, if grouped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
}

/// The autosave state of one open document.
#[derive(Debug, Clone)]
pub struct Autosave {
    doc_key: StorageKey,
    rows: Vec<JournalRow>,
    last_snapshot_s: f64,
}

impl Autosave {
    /// Autosave for the document stored at `doc_key` (an untitled document
    /// gets a key of the frontend's choosing); `now_s` starts the timer.
    pub fn new(doc_key: StorageKey, now_s: f64) -> Self {
        Autosave {
            doc_key,
            rows: Vec::new(),
            last_snapshot_s: now_s,
        }
    }

    /// Restarts the snapshot timer at `now_s` (when the frontend's clock
    /// starts later than the autosave).
    pub fn restart_clock(&mut self, now_s: f64) {
        self.last_snapshot_s = now_s;
    }

    /// The document's key.
    pub fn doc_key(&self) -> &StorageKey {
        &self.doc_key
    }

    /// Commands journaled since the last snapshot.
    pub fn journaled(&self) -> usize {
        self.rows.len()
    }

    /// Appends a committed command to the journal.
    pub fn record(
        &mut self,
        storage: &dyn Storage,
        command: &str,
        params: &serde_json::Value,
        group: Option<&str>,
    ) -> Result<(), IoError> {
        self.rows.push(JournalRow {
            seq: self.rows.len() as u64 + 1,
            command: command.to_string(),
            params: params.clone(),
            group: group.map(str::to_string),
        });
        storage.write(&self.doc_key.journal(), &encode_journal(&self.rows))
    }

    /// `true` when a snapshot is due at `now_s`.
    pub fn due(&self, now_s: f64) -> bool {
        !self.rows.is_empty()
            && (self.rows.len() >= SNAPSHOT_EVERY_COMMANDS
                || now_s - self.last_snapshot_s >= SNAPSHOT_EVERY_SECONDS)
    }

    /// Writes `bytes` (the whole document, journal included) as the snapshot
    /// and empties the journal.
    pub fn snapshot(
        &mut self,
        storage: &dyn Storage,
        bytes: &[u8],
        now_s: f64,
    ) -> Result<(), IoError> {
        storage.write(&self.doc_key.autosave(), bytes)?;
        storage.delete(&self.doc_key.journal())?;
        self.rows.clear();
        self.last_snapshot_s = now_s;
        Ok(())
    }

    /// Deletes the snapshot and the journal (after an explicit save, or when
    /// the user discards a recovery) and moves to `doc_key`.
    pub fn reset(
        &mut self,
        storage: &dyn Storage,
        doc_key: StorageKey,
        now_s: f64,
    ) -> Result<(), IoError> {
        discard(storage, &self.doc_key)?;
        if doc_key != self.doc_key {
            discard(storage, &doc_key)?;
        }
        *self = Autosave::new(doc_key, now_s);
        Ok(())
    }
}

/// `true` when unsaved work of the document at `doc_key` can be recovered.
pub fn pending(storage: &dyn Storage, doc_key: &StorageKey) -> bool {
    storage.exists(&doc_key.autosave()).unwrap_or(false)
        || storage.exists(&doc_key.journal()).unwrap_or(false)
}

/// Deletes the snapshot and the journal of the document at `doc_key`.
pub fn discard(storage: &dyn Storage, doc_key: &StorageKey) -> Result<(), IoError> {
    storage.delete(&doc_key.autosave())?;
    storage.delete(&doc_key.journal())
}

/// A recovered document.
pub struct Recovered {
    /// The engine holding the recovered document (with `storage` set).
    pub engine: Engine,
    /// The file the replay started from (the snapshot, else the document),
    /// for what a later save carries over.
    pub loaded: Option<LoadedDocument>,
    /// Its history plus the replayed commands.
    pub history: Vec<HistoryEntry>,
    /// Commands replayed.
    pub replayed: usize,
    /// Commands that failed to replay (`command: error`), skipped.
    pub skipped: Vec<String>,
}

/// Rebuilds the unsaved state of the document at `doc_key`: the snapshot
/// (else the document, else an empty one) with the journal replayed.
pub fn recover(
    storage: Arc<dyn Storage + Send + Sync>,
    doc_key: &StorageKey,
    registry: Arc<CommandRegistry>,
) -> Result<Recovered, IoError> {
    let base = [doc_key.autosave(), doc_key.clone()]
        .into_iter()
        .find(|k| storage.exists(k).unwrap_or(false));
    let mut loaded = match base {
        Some(k) => Some(read_document(&storage.read(&k)?, &NoProgress)?),
        None => None,
    };
    let doc = loaded
        .as_mut()
        .map(|l| std::mem::take(&mut l.document))
        .unwrap_or_else(Document::new);
    let mut history = loaded
        .as_ref()
        .map(|l| l.history.clone())
        .unwrap_or_default();
    let rows = match storage.read(&doc_key.journal()) {
        Ok(bytes) => decode_journal(&bytes)?,
        Err(IoError::NotFound(_)) => Vec::new(),
        Err(e) => return Err(e),
    };
    let mut engine = Engine::new(doc, registry);
    engine.set_storage(storage);
    if let Some(last) = history.last() {
        engine.set_last_created(last.created.clone());
    }
    let (mut replayed, mut skipped) = (0, Vec::new());
    for row in rows {
        match engine.execute_erased(&row.command, row.params.clone()) {
            Ok(_) => {
                replayed += 1;
                history.push(HistoryEntry {
                    command: row.command,
                    params: row.params,
                    group: row.group,
                    timestamp: None,
                    created: engine.last_created().to_vec(),
                });
            }
            Err(e) => skipped.push(format!("{}: {e}", row.command)),
        }
    }
    Ok(Recovered {
        engine,
        loaded,
        history,
        replayed,
        skipped,
    })
}

/// The journal as JSON lines.
pub fn encode_journal(rows: &[JournalRow]) -> Vec<u8> {
    let mut out = Vec::new();
    for row in rows {
        // A row of JSON values always serializes.
        if let Ok(line) = serde_json::to_vec(row) {
            out.extend_from_slice(&line);
            out.push(b'\n');
        }
    }
    out
}

/// Reads JSON lines; a torn last line (a write cut short) is dropped.
pub fn decode_journal(bytes: &[u8]) -> Result<Vec<JournalRow>, IoError> {
    let lines: Vec<&[u8]> = bytes
        .split(|b| *b == b'\n')
        .filter(|l| !l.iter().all(u8::is_ascii_whitespace))
        .collect();
    let mut rows = Vec::with_capacity(lines.len());
    for (i, line) in lines.iter().enumerate() {
        match serde_json::from_slice(line) {
            Ok(row) => rows.push(row),
            Err(_) if i + 1 == lines.len() => break,
            Err(e) => {
                return Err(IoError::corrupt("journal", format!("line {}: {e}", i + 1)));
            }
        }
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use serde_json::json;

    use super::*;
    use crate::io::MemoryStorage;
    use crate::io::native::{WriteOptions, write_document};

    fn registry() -> Arc<CommandRegistry> {
        Arc::new(CommandRegistry::with_builtins())
    }

    fn rect(x: f64) -> serde_json::Value {
        json!({ "corner_a": [x, 0.0, 0.0], "corner_b": [x + 1000.0, 1000.0, 0.0] })
    }

    #[test]
    fn journal_and_snapshot_recover_the_session() {
        let storage: Arc<dyn Storage + Send + Sync> = Arc::new(MemoryStorage::new());
        let key = StorageKey::new("room.plyr");
        let mut live = Engine::new(Document::new(), registry());
        let mut autosave = Autosave::new(key.clone(), 0.0);
        let mut history = Vec::new();
        for i in 0..3 {
            let params = rect(f64::from(i) * 2000.0);
            live.execute_erased("rectangle", params.clone()).unwrap();
            autosave
                .record(storage.as_ref(), "rectangle", &params, None)
                .unwrap();
            history.push(HistoryEntry {
                command: "rectangle".into(),
                params,
                group: None,
                timestamp: None,
                created: live.last_created().to_vec(),
            });
            if i == 1 {
                assert!(!autosave.due(10.0));
                assert!(autosave.due(SNAPSHOT_EVERY_SECONDS));
                let bytes = write_document(
                    live.document(),
                    &history,
                    &WriteOptions::default(),
                    &NoProgress,
                )
                .unwrap();
                autosave
                    .snapshot(storage.as_ref(), &bytes, SNAPSHOT_EVERY_SECONDS)
                    .unwrap();
                assert_eq!(autosave.journaled(), 0);
            }
        }
        assert!(pending(storage.as_ref(), &key));
        let back = recover(Arc::clone(&storage), &key, registry()).unwrap();
        assert_eq!((back.replayed, back.skipped.len()), (1, 0));
        assert_eq!(back.history.len(), 3);
        assert_eq!(
            back.engine.document().model.mesh().face_count(),
            live.document().model.mesh().face_count()
        );
        autosave.reset(storage.as_ref(), key.clone(), 0.0).unwrap();
        assert!(!pending(storage.as_ref(), &key));
    }

    #[test]
    fn a_journal_without_snapshot_replays_on_the_document() {
        let storage: Arc<dyn Storage + Send + Sync> = Arc::new(MemoryStorage::new());
        let key = StorageKey::new("plan.plyr");
        let mut saved = Engine::new(Document::new(), registry());
        saved.execute_erased("rectangle", rect(0.0)).unwrap();
        let bytes =
            write_document(saved.document(), &[], &WriteOptions::default(), &NoProgress).unwrap();
        storage.write(&key, &bytes).unwrap();
        let mut autosave = Autosave::new(key.clone(), 0.0);
        autosave
            .record(storage.as_ref(), "rectangle", &rect(5000.0), None)
            .unwrap();
        autosave
            .record(storage.as_ref(), "no-such-command", &json!({}), None)
            .unwrap();
        let back = recover(Arc::clone(&storage), &key, registry()).unwrap();
        assert_eq!((back.replayed, back.skipped.len()), (1, 1));
        assert_eq!(back.engine.document().model.mesh().face_count(), 2);
        assert_eq!(
            storage.read(&key).unwrap(),
            bytes,
            "the document is untouched"
        );
    }

    #[test]
    fn a_torn_last_line_is_dropped() {
        let rows = vec![JournalRow {
            seq: 1,
            command: "line".into(),
            params: json!({ "points": [[0, 0, 0], [1, 0, 0]] }),
            group: None,
        }];
        let mut bytes = encode_journal(&rows);
        bytes.extend_from_slice(b"{\"seq\":2,\"comm");
        assert_eq!(decode_journal(&bytes).unwrap(), rows);
        assert!(decode_journal(b"garbage\n{}\n").is_err());
    }
}

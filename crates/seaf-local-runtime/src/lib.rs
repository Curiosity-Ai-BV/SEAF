use std::{error::Error, fmt, path::Path};

use rusqlite::{params, Connection};
use seaf_core::{validate_seaf_event, FieldError, PrivacyLevel, SeafEvent, Signal, SignalSeverity};

pub fn runtime_component() -> &'static str {
    "seaf-local-runtime"
}

pub struct EventStore {
    connection: Connection,
}

impl EventStore {
    pub fn open(path: &Path) -> Result<Self, RuntimeError> {
        let connection = Connection::open(path)?;
        let store = Self { connection };
        store.initialize()?;
        Ok(store)
    }

    pub fn in_memory() -> Result<Self, RuntimeError> {
        let connection = Connection::open_in_memory()?;
        let store = Self { connection };
        store.initialize()?;
        Ok(store)
    }

    pub fn ingest_event(&self, event: &SeafEvent) -> Result<(), RuntimeError> {
        let errors = validate_seaf_event(event);
        if !errors.is_empty() {
            return Err(RuntimeError::Validation(errors));
        }

        let payload_json = serde_json::to_string(&event.payload)?;
        self.connection.execute(
            "INSERT INTO events (event_id, name, timestamp, source, privacy_level, payload_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                event.event_id,
                event.name,
                event.timestamp,
                event.source,
                privacy_level_label(event.privacy_level),
                payload_json
            ],
        )?;

        Ok(())
    }

    pub fn ingest_event_json(&self, event_json: &str) -> Result<SeafEvent, RuntimeError> {
        let event: SeafEvent = serde_json::from_str(event_json)?;
        self.ingest_event(&event)?;
        Ok(event)
    }

    pub fn list_events(&self) -> Result<Vec<SeafEvent>, RuntimeError> {
        let mut statement = self.connection.prepare(
            "SELECT event_id, name, timestamp, source, privacy_level, payload_json
             FROM events
             ORDER BY rowid ASC",
        )?;
        let rows = statement.query_map([], |row| {
            let privacy_label: String = row.get(4)?;
            let payload_json: String = row.get(5)?;
            let payload = serde_json::from_str(&payload_json).map_err(|err| {
                rusqlite::Error::FromSqlConversionFailure(
                    payload_json.len(),
                    rusqlite::types::Type::Text,
                    Box::new(err),
                )
            })?;

            Ok(SeafEvent {
                event_id: row.get(0)?,
                name: row.get(1)?,
                timestamp: row.get(2)?,
                source: row.get(3)?,
                privacy_level: parse_privacy_level(&privacy_label).ok_or_else(|| {
                    rusqlite::Error::FromSqlConversionFailure(
                        privacy_label.len(),
                        rusqlite::types::Type::Text,
                        Box::new(RuntimeError::InvalidPrivacyLevel(privacy_label.clone())),
                    )
                })?,
                payload,
            })
        })?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(RuntimeError::from)
    }

    pub fn summarize_abandonment(
        &self,
        goal_id: &str,
        started_event: &str,
        completed_event: &str,
        min_abandoned_sessions: u64,
    ) -> Result<Option<Signal>, RuntimeError> {
        let started_count = self.count_events(started_event)?;
        let completed_count = self.count_events(completed_event)?;
        let abandoned_count = started_count.saturating_sub(completed_count);

        if abandoned_count < min_abandoned_sessions {
            return Ok(None);
        }

        Ok(Some(Signal {
            signal_id: format!("sig_{goal_id}_{started_event}_without_{completed_event}")
                .replace('.', "_"),
            source: "local_runtime".to_string(),
            signal_type: "workflow_abandonment".to_string(),
            related_goal_id: Some(goal_id.to_string()),
            summary: format!(
                "{abandoned_count} observed {started_event} event(s) did not reach {completed_event}."
            ),
            severity: SignalSeverity::Medium,
            privacy_level: PrivacyLevel::Aggregated,
            evidence: serde_json::json!({
                "started_event": started_event,
                "completed_event": completed_event,
                "started_count": started_count,
                "completed_count": completed_count,
                "abandoned_count": abandoned_count
            }),
        }))
    }

    fn initialize(&self) -> Result<(), RuntimeError> {
        self.connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS events (
                event_id TEXT PRIMARY KEY NOT NULL,
                name TEXT NOT NULL,
                timestamp TEXT NOT NULL,
                source TEXT NOT NULL,
                privacy_level TEXT NOT NULL,
                payload_json TEXT NOT NULL
            );",
        )?;
        Ok(())
    }

    fn count_events(&self, name: &str) -> Result<u64, RuntimeError> {
        let count = self.connection.query_row(
            "SELECT COUNT(*) FROM events WHERE name = ?1",
            params![name],
            |row| row.get::<_, u64>(0),
        )?;
        Ok(count)
    }
}

#[derive(Debug)]
pub enum RuntimeError {
    Database(rusqlite::Error),
    Json(serde_json::Error),
    Validation(Vec<FieldError>),
    InvalidPrivacyLevel(String),
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Database(error) => write!(formatter, "database error: {error}"),
            Self::Json(error) => write!(formatter, "json error: {error}"),
            Self::Validation(errors) => write!(formatter, "validation failed: {errors:?}"),
            Self::InvalidPrivacyLevel(value) => write!(formatter, "invalid privacy level: {value}"),
        }
    }
}

impl Error for RuntimeError {}

impl From<rusqlite::Error> for RuntimeError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Database(error)
    }
}

impl From<serde_json::Error> for RuntimeError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

fn privacy_level_label(level: PrivacyLevel) -> &'static str {
    match level {
        PrivacyLevel::Public => "public",
        PrivacyLevel::Aggregated => "aggregated",
        PrivacyLevel::Private => "private",
        PrivacyLevel::Sensitive => "sensitive",
    }
}

fn parse_privacy_level(value: &str) -> Option<PrivacyLevel> {
    match value {
        "public" => Some(PrivacyLevel::Public),
        "aggregated" => Some(PrivacyLevel::Aggregated),
        "private" => Some(PrivacyLevel::Private),
        "sensitive" => Some(PrivacyLevel::Sensitive),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposes_runtime_component_name() {
        assert_eq!(runtime_component(), "seaf-local-runtime");
    }

    #[test]
    fn stores_and_reads_valid_events() {
        let store = EventStore::in_memory().expect("store");
        let event = note_event("evt_1", "note.created");

        store.ingest_event(&event).expect("ingest event");
        let events = store.list_events().expect("list events");

        assert_eq!(events, vec![event]);
    }

    #[test]
    fn rejects_malformed_events_before_persistence() {
        let store = EventStore::in_memory().expect("store");
        let mut event = note_event("evt_1", "note.created");
        event.payload = serde_json::json!("raw private text");

        let error = store.ingest_event(&event).unwrap_err();

        assert!(matches!(error, RuntimeError::Validation(_)));
        assert!(store.list_events().expect("list events").is_empty());
    }

    #[test]
    fn ingests_sdk_event_json() {
        let store = EventStore::in_memory().expect("store");
        let event = store
            .ingest_event_json(
                r#"{
  "event_id": "evt_1",
  "name": "note.created",
  "timestamp": "2026-06-30T00:00:00.000Z",
  "source": "adaptive-notes",
  "privacy_level": "aggregated",
  "payload": { "source": "empty_state_button" }
}"#,
            )
            .expect("ingest sdk json");

        assert_eq!(event.name, "note.created");
        assert_eq!(store.list_events().expect("list events"), vec![event]);
    }

    #[test]
    fn summarizes_abandonment_without_raw_payloads() {
        let store = EventStore::in_memory().expect("store");
        store
            .ingest_event(&note_event("evt_1", "onboarding.started"))
            .expect("ingest start");
        store
            .ingest_event(&note_event("evt_2", "onboarding.started"))
            .expect("ingest start");
        store
            .ingest_event(&note_event("evt_3", "onboarding.completed"))
            .expect("ingest complete");

        let signal = store
            .summarize_abandonment(
                "reduce_time_to_first_note",
                "onboarding.started",
                "onboarding.completed",
                1,
            )
            .expect("summarize")
            .expect("signal");

        assert_eq!(signal.privacy_level, PrivacyLevel::Aggregated);
        assert_eq!(signal.evidence["abandoned_count"], 1);
        assert!(signal.evidence.get("payload").is_none());
    }

    fn note_event(event_id: &str, name: &str) -> SeafEvent {
        SeafEvent {
            event_id: event_id.to_string(),
            name: name.to_string(),
            timestamp: "2026-06-30T00:00:00.000Z".to_string(),
            source: "adaptive-notes".to_string(),
            privacy_level: PrivacyLevel::Aggregated,
            payload: serde_json::json!({ "flow": "first_run" }),
        }
    }
}

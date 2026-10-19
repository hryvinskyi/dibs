//! The project's append-only event log.
//!
//! An event is stored as its kind's name and its fields: the two halves of the form the domain's
//! serde derive writes (`{"kind": …, "payload": …}`), put back together when it is read.

use async_trait::async_trait;
use serde_json::{Map, Value};
use sqlx::sqlite::SqliteRow;
use time::OffsetDateTime;

use dibs_app::ports::{EventRows, StoreError, StoreResult};
use dibs_domain::event::{Event, EventKind};
use dibs_domain::ids::{EventId, ProjectId};

use crate::SqliteTx;
use crate::codec::{Fields, int, json, nanos};
use crate::error::unstorable;

const KIND: &str = "kind";
const PAYLOAD: &str = "payload";

/// The kind's name and its fields as JSON text.
fn split(kind: &EventKind) -> StoreResult<(String, String)> {
    let value = serde_json::to_value(kind).map_err(|error| StoreError::Backend(Box::new(error)))?;
    match (value.get(KIND), value.get(PAYLOAD)) {
        (Some(Value::String(name)), Some(payload)) => Ok((name.clone(), json(payload)?)),
        _ => Err(unstorable("an event without a kind and a payload")),
    }
}

fn event(row: &SqliteRow) -> StoreResult<Event> {
    let f = Fields::of(row, "events");
    let payload: Value = f.json(PAYLOAD)?;
    let mut whole = Map::new();
    whole.insert(KIND.to_owned(), Value::String(f.text(KIND)?));
    whole.insert(PAYLOAD.to_owned(), payload);
    let kind =
        serde_json::from_value(Value::Object(whole)).map_err(|error| f.bad(PAYLOAD, error))?;
    Ok(Event {
        id: EventId(f.number("id")?),
        project: ProjectId(f.number("project_id")?),
        at: f.time("at")?,
        kind,
    })
}

#[async_trait]
impl EventRows for SqliteTx {
    async fn append_event(
        &mut self,
        project: ProjectId,
        at: OffsetDateTime,
        kind: &EventKind,
    ) -> StoreResult<EventId> {
        let (name, payload) = split(kind)?;
        let statement = sqlx::query(
            "INSERT INTO events (project_id, kind, payload, at) VALUES (?1, ?2, ?3, ?4) \
             RETURNING id",
        )
        .bind(int(project.get())?)
        .bind(name)
        .bind(payload)
        .bind(nanos(at)?);
        let row = self.one(statement).await?;
        Ok(EventId(Fields::of(&row, "events").number("id")?))
    }

    async fn events_after(
        &mut self,
        project: ProjectId,
        after: Option<EventId>,
        limit: usize,
    ) -> StoreResult<Vec<Event>> {
        let statement = sqlx::query(
            "SELECT id, project_id, kind, payload, at FROM events \
             WHERE project_id = ?1 AND (?2 IS NULL OR id > ?2) ORDER BY id LIMIT ?3",
        )
        .bind(int(project.get())?)
        .bind(after.map(|id| int(id.get())).transpose()?)
        .bind(i64::try_from(limit).unwrap_or(i64::MAX));
        self.all(statement).await?.iter().map(event).collect()
    }

    async fn latest_event(&mut self, project: ProjectId) -> StoreResult<Option<EventId>> {
        let statement = sqlx::query("SELECT MAX(id) AS id FROM events WHERE project_id = ?1")
            .bind(int(project.get())?);
        let row = self.one(statement).await?;
        Ok(Fields::of(&row, "events").opt_number("id")?.map(EventId))
    }
}

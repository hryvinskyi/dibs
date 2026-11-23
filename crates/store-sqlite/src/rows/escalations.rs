//! The escalations members sent to the arbiters.

use std::collections::BTreeSet;

use async_trait::async_trait;
use sqlx::sqlite::SqliteRow;

use dibs_app::ports::{EscalationRows, StoreResult};
use dibs_app::records::EscalationRecord;
use dibs_domain::ids::{ItemId, MemberId, MessageId, ProjectId, ThreadId};

use crate::SqliteTx;
use crate::codec::{Fields, int, json, nanos};

fn escalation(row: &SqliteRow) -> StoreResult<EscalationRecord> {
    let f = Fields::of(row, "escalations");
    let arbiters: BTreeSet<MemberId> = f
        .json::<Vec<u64>>("arbiters")?
        .into_iter()
        .map(MemberId)
        .collect();
    let ties: BTreeSet<[ItemId; 2]> = f
        .json::<Vec<[u64; 2]>>("ties")?
        .into_iter()
        .map(|pair| pair.map(ItemId))
        .collect();
    Ok(EscalationRecord {
        message: MessageId(f.number("message_id")?),
        thread: ThreadId(f.number("thread_id")?),
        subject: f.json("subject")?,
        by: MemberId(f.number("by_id")?),
        raised_by_dibs: f.flag("raised_by_dibs")?,
        ties,
        arbiters,
        created_at: f.time("created_at")?,
    })
}

#[async_trait]
impl EscalationRows for SqliteTx {
    async fn insert_escalation(
        &mut self,
        project: ProjectId,
        record: &EscalationRecord,
    ) -> StoreResult<()> {
        let arbiters: Vec<u64> = record.arbiters.iter().map(|member| member.get()).collect();
        let ties: Vec<[u64; 2]> = record
            .ties
            .iter()
            .map(|pair| pair.map(ItemId::get))
            .collect();
        let statement = sqlx::query(
            "INSERT INTO escalations (project_id, message_id, thread_id, subject, by_id, \
             raised_by_dibs, ties, arbiters, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        )
        .bind(int(project.get())?)
        .bind(int(record.message.get())?)
        .bind(int(record.thread.get())?)
        .bind(json(&record.subject)?)
        .bind(int(record.by.get())?)
        .bind(i64::from(record.raised_by_dibs))
        .bind(json(&ties)?)
        .bind(json(&arbiters)?)
        .bind(nanos(record.created_at)?);
        self.run(statement).await?;
        Ok(())
    }

    async fn open_escalations(&mut self, project: ProjectId) -> StoreResult<Vec<EscalationRecord>> {
        let statement = sqlx::query(
            "SELECT e.message_id, e.thread_id, e.subject, e.by_id, e.raised_by_dibs, e.ties, \
             e.arbiters, e.created_at \
             FROM escalations e WHERE e.project_id = ?1 AND NOT EXISTS (\
               SELECT 1 FROM acks a, json_each(e.arbiters) j \
               WHERE a.project_id = e.project_id AND a.message_id = e.message_id \
               AND a.member_id = j.value) \
             ORDER BY e.created_at DESC, e.message_id DESC",
        )
        .bind(int(project.get())?);
        self.all(statement).await?.iter().map(escalation).collect()
    }

    async fn add_escalation_tie(
        &mut self,
        project: ProjectId,
        message: MessageId,
        tie: [ItemId; 2],
    ) -> StoreResult<()> {
        let statement = sqlx::query(
            "UPDATE escalations SET ties = json_insert(ties, '$[#]', json(?3)) \
             WHERE project_id = ?1 AND message_id = ?2 \
             AND NOT EXISTS (SELECT 1 FROM json_each(escalations.ties) t WHERE t.value = json(?3))",
        )
        .bind(int(project.get())?)
        .bind(int(message.get())?)
        .bind(json(&tie.map(ItemId::get))?);
        self.run(statement).await?;
        Ok(())
    }
}

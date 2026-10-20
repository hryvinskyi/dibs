//! The announcements wide claims sent to the holders of their areas.

use async_trait::async_trait;
use sqlx::sqlite::SqliteRow;

use dibs_app::ports::{StoreResult, WideAskRows};
use dibs_domain::ids::{ItemId, MemberId, MessageId, ProjectId};
use dibs_domain::wide::WideAsk;

use crate::SqliteTx;
use crate::codec::{Fields, int, json};

fn wide_ask(row: &SqliteRow) -> StoreResult<WideAsk> {
    let f = Fields::of(row, "wide_asks");
    Ok(WideAsk {
        message: MessageId(f.number("message_id")?),
        item: ItemId(f.number("item_id")?),
        from: MemberId(f.number("from_id")?),
        to: MemberId(f.number("to_id")?),
        areas: f.json("areas")?,
    })
}

#[async_trait]
impl WideAskRows for SqliteTx {
    async fn insert_wide_ask(&mut self, project: ProjectId, ask: &WideAsk) -> StoreResult<()> {
        let statement = sqlx::query(
            "INSERT INTO wide_asks (project_id, message_id, item_id, from_id, to_id, areas) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )
        .bind(int(project.get())?)
        .bind(int(ask.message.get())?)
        .bind(int(ask.item.get())?)
        .bind(int(ask.from.get())?)
        .bind(int(ask.to.get())?)
        .bind(json(&ask.areas)?);
        self.run(statement).await?;
        Ok(())
    }

    async fn wide_ask(
        &mut self,
        project: ProjectId,
        message: MessageId,
    ) -> StoreResult<Option<WideAsk>> {
        let statement = sqlx::query(
            "SELECT message_id, item_id, from_id, to_id, areas FROM wide_asks \
             WHERE project_id = ?1 AND message_id = ?2",
        )
        .bind(int(project.get())?)
        .bind(int(message.get())?);
        self.optional(statement)
            .await?
            .as_ref()
            .map(wide_ask)
            .transpose()
    }

    async fn open_wide_asks(&mut self, project: ProjectId) -> StoreResult<Vec<WideAsk>> {
        let statement = sqlx::query(
            "SELECT w.message_id, w.item_id, w.from_id, w.to_id, w.areas FROM wide_asks w \
             WHERE w.project_id = ?1 AND NOT EXISTS (SELECT 1 FROM acks a \
                 WHERE a.project_id = w.project_id AND a.message_id = w.message_id \
                 AND a.member_id = w.to_id) \
             ORDER BY w.message_id",
        )
        .bind(int(project.get())?);
        self.all(statement).await?.iter().map(wide_ask).collect()
    }
}

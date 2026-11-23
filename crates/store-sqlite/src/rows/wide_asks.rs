//! The announcements wide claims sent to the holders of their areas.

use async_trait::async_trait;
use sqlx::sqlite::SqliteRow;
use time::OffsetDateTime;

use dibs_app::ports::{StoreResult, WideAskRows};
use dibs_app::records::WideAskRecord;
use dibs_domain::grant::GrantDecision;
use dibs_domain::ids::{ItemId, MemberId, MessageId, ProjectId};
use dibs_domain::wide::WideAsk;

use crate::SqliteTx;
use crate::codec::{Fields, int, json, nanos};
use crate::error::unstorable;

fn wide_ask(row: &SqliteRow) -> StoreResult<WideAsk> {
    let f = Fields::of(row, "wide_asks");
    Ok(WideAsk {
        message: MessageId(f.number("message_id")?),
        item: ItemId(f.number("item_id")?),
        from: MemberId(f.number("from_id")?),
        to: MemberId(f.number("to_id")?),
        areas: f.json("areas")?,
        acked_by_dibs: f.opt_parse("dibs_acked_because", GrantDecision::parse)?,
    })
}

/// The columns an announcement is read from, each named for its table `w`.
macro_rules! wide_ask_columns {
    () => {
        "w.message_id, w.item_id, w.from_id, w.to_id, w.areas, w.dibs_acked_because"
    };
}

/// The condition that leaves out every announcement acknowledged by its holder or by dibs.
macro_rules! open_condition {
    () => {
        "w.dibs_acked_at IS NULL AND NOT EXISTS (SELECT 1 FROM acks a \
         WHERE a.project_id = w.project_id AND a.message_id = w.message_id \
         AND a.member_id = w.to_id)"
    };
}

#[async_trait]
impl WideAskRows for SqliteTx {
    async fn insert_wide_ask(&mut self, project: ProjectId, ask: &WideAsk) -> StoreResult<()> {
        if ask.acked_by_dibs.is_some() {
            return Err(unstorable(
                "a new announcement already acknowledged by dibs",
            ));
        }
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
        let statement = sqlx::query(concat!(
            "SELECT ",
            wide_ask_columns!(),
            " FROM wide_asks w WHERE w.project_id = ?1 AND w.message_id = ?2"
        ))
        .bind(int(project.get())?)
        .bind(int(message.get())?);
        self.optional(statement)
            .await?
            .as_ref()
            .map(wide_ask)
            .transpose()
    }

    async fn open_wide_asks(&mut self, project: ProjectId) -> StoreResult<Vec<WideAsk>> {
        let statement = sqlx::query(concat!(
            "SELECT ",
            wide_ask_columns!(),
            " FROM wide_asks w WHERE w.project_id = ?1 AND ",
            open_condition!(),
            " ORDER BY w.message_id"
        ))
        .bind(int(project.get())?);
        self.all(statement).await?.iter().map(wide_ask).collect()
    }

    async fn waiting_wide_asks(&mut self, project: ProjectId) -> StoreResult<Vec<WideAsk>> {
        let statement = sqlx::query(concat!(
            "SELECT ",
            wide_ask_columns!(),
            " FROM wide_asks w WHERE w.project_id = ?1 AND w.settled_at IS NULL AND ",
            open_condition!(),
            " ORDER BY w.message_id"
        ))
        .bind(int(project.get())?);
        self.all(statement).await?.iter().map(wide_ask).collect()
    }

    async fn open_wide_asks_with_time(
        &mut self,
        project: ProjectId,
    ) -> StoreResult<Vec<(WideAskRecord, OffsetDateTime)>> {
        let statement = sqlx::query(concat!(
            "SELECT ",
            wide_ask_columns!(),
            ", w.left_to_holder, sent.created_at FROM wide_asks w \
             JOIN messages sent ON sent.project_id = w.project_id AND sent.id = w.message_id \
             WHERE w.project_id = ?1 AND w.settled_at IS NULL AND ",
            open_condition!(),
            " ORDER BY w.message_id"
        ))
        .bind(int(project.get())?);
        self.all(statement)
            .await?
            .iter()
            .map(|row| {
                let sent_at = Fields::of(row, "messages").time("created_at")?;
                let record = WideAskRecord {
                    ask: wide_ask(row)?,
                    left_to_holder: Fields::of(row, "wide_asks").flag("left_to_holder")?,
                };
                Ok((record, sent_at))
            })
            .collect()
    }

    async fn mark_wide_ask_acked_by_dibs(
        &mut self,
        project: ProjectId,
        message: MessageId,
        at: OffsetDateTime,
        because: GrantDecision,
    ) -> StoreResult<()> {
        if !because.is_by_dibs() {
            return Err(unstorable(
                "an acknowledgement by dibs that names no reason of dibs",
            ));
        }
        let statement = sqlx::query(
            "UPDATE wide_asks SET dibs_acked_at = ?3, dibs_acked_because = ?4 \
             WHERE project_id = ?1 AND message_id = ?2 AND dibs_acked_at IS NULL \
             AND settled_at IS NULL",
        )
        .bind(int(project.get())?)
        .bind(int(message.get())?)
        .bind(nanos(at)?)
        .bind(because.code());
        self.run(statement).await?;
        Ok(())
    }

    async fn leave_wide_ask_to_holder(
        &mut self,
        project: ProjectId,
        message: MessageId,
    ) -> StoreResult<()> {
        let statement = sqlx::query(
            "UPDATE wide_asks SET left_to_holder = 1 WHERE project_id = ?1 AND message_id = ?2",
        )
        .bind(int(project.get())?)
        .bind(int(message.get())?);
        self.run(statement).await?;
        Ok(())
    }

    async fn settle_wide_ask(
        &mut self,
        project: ProjectId,
        message: MessageId,
        at: OffsetDateTime,
    ) -> StoreResult<()> {
        let statement = sqlx::query(
            "UPDATE wide_asks SET settled_at = ?3 WHERE project_id = ?1 AND message_id = ?2 \
             AND settled_at IS NULL AND dibs_acked_at IS NULL",
        )
        .bind(int(project.get())?)
        .bind(int(message.get())?)
        .bind(nanos(at)?);
        self.run(statement).await?;
        Ok(())
    }
}

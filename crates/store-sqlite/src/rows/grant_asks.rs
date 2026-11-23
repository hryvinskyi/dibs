//! The grants members asked of the holders of areas.

use async_trait::async_trait;
use sqlx::sqlite::SqliteRow;
use time::OffsetDateTime;

use dibs_app::ports::{AskedGrantRows, StoreResult};
use dibs_app::records::AskedGrantRecord;
use dibs_domain::area::{AreaName, QualifiedArea};
use dibs_domain::grant::{AskedGrant, GrantAsk};
use dibs_domain::ids::{ItemId, MemberId, MessageId, ProjectId};
use dibs_domain::repo::RepoId;

use crate::SqliteTx;
use crate::codec::{Fields, int, nanos};

fn asked_grant(row: &SqliteRow) -> StoreResult<AskedGrant> {
    let f = Fields::of(row, "grant_asks");
    Ok(AskedGrant {
        message: MessageId(f.number("message_id")?),
        ask: GrantAsk {
            asker: MemberId(f.number("asker_id")?),
            item: ItemId(f.number("item_id")?),
            area: QualifiedArea {
                repo: f.parse("repo", RepoId::parse)?,
                name: f.parse("area", AreaName::parse)?,
            },
        },
        holder: MemberId(f.number("holder_id")?),
    })
}

fn asked_grant_record(row: &SqliteRow) -> StoreResult<AskedGrantRecord> {
    Ok(AskedGrantRecord {
        asked: asked_grant(row)?,
        left_to_holder: Fields::of(row, "grant_asks").flag("left_to_holder")?,
    })
}

#[async_trait]
impl AskedGrantRows for SqliteTx {
    async fn insert_asked_grant(
        &mut self,
        project: ProjectId,
        asked: &AskedGrant,
    ) -> StoreResult<()> {
        let statement = sqlx::query(
            "INSERT INTO grant_asks (project_id, message_id, asker_id, item_id, repo, area, \
             holder_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )
        .bind(int(project.get())?)
        .bind(int(asked.message.get())?)
        .bind(int(asked.ask.asker.get())?)
        .bind(int(asked.ask.item.get())?)
        .bind(asked.ask.area.repo.as_str().to_owned())
        .bind(asked.ask.area.name.as_str().to_owned())
        .bind(int(asked.holder.get())?);
        self.run(statement).await?;
        Ok(())
    }

    async fn unanswered_asked_grants(
        &mut self,
        project: ProjectId,
        holder: MemberId,
    ) -> StoreResult<Vec<AskedGrantRecord>> {
        let statement = sqlx::query(
            "SELECT g.message_id, g.asker_id, g.item_id, g.repo, g.area, g.holder_id, \
             g.left_to_holder FROM grant_asks g \
             JOIN messages asking ON asking.project_id = g.project_id AND asking.id = g.message_id \
             WHERE g.project_id = ?1 AND g.holder_id = ?2 AND g.settled_at IS NULL \
             AND NOT EXISTS (SELECT 1 FROM messages m \
                 WHERE m.project_id = g.project_id AND m.thread_id = asking.thread_id \
                 AND m.author_id = g.holder_id) \
             ORDER BY g.message_id",
        )
        .bind(int(project.get())?)
        .bind(int(holder.get())?);
        self.all(statement)
            .await?
            .iter()
            .map(asked_grant_record)
            .collect()
    }

    async fn unsettled_asked_grants(
        &mut self,
        project: ProjectId,
    ) -> StoreResult<Vec<(AskedGrantRecord, OffsetDateTime)>> {
        let statement = sqlx::query(
            "SELECT g.message_id, g.asker_id, g.item_id, g.repo, g.area, g.holder_id, \
             g.left_to_holder, asking.created_at FROM grant_asks g \
             JOIN messages asking ON asking.project_id = g.project_id AND asking.id = g.message_id \
             WHERE g.project_id = ?1 AND g.settled_at IS NULL \
             ORDER BY g.message_id",
        )
        .bind(int(project.get())?);
        self.all(statement)
            .await?
            .iter()
            .map(|row| {
                let asked_at = Fields::of(row, "messages").time("created_at")?;
                Ok((asked_grant_record(row)?, asked_at))
            })
            .collect()
    }

    async fn settle_asked_grant(
        &mut self,
        project: ProjectId,
        message: MessageId,
        at: OffsetDateTime,
    ) -> StoreResult<()> {
        let statement = sqlx::query(
            "UPDATE grant_asks SET settled_at = ?3 \
             WHERE project_id = ?1 AND message_id = ?2 AND settled_at IS NULL",
        )
        .bind(int(project.get())?)
        .bind(int(message.get())?)
        .bind(nanos(at)?);
        self.run(statement).await?;
        Ok(())
    }

    async fn leave_asked_grant_to_holder(
        &mut self,
        project: ProjectId,
        message: MessageId,
    ) -> StoreResult<()> {
        let statement = sqlx::query(
            "UPDATE grant_asks SET left_to_holder = 1 WHERE project_id = ?1 AND message_id = ?2",
        )
        .bind(int(project.get())?)
        .bind(int(message.get())?);
        self.run(statement).await?;
        Ok(())
    }

    async fn settle_asked_grants_for_item(
        &mut self,
        project: ProjectId,
        asked: &GrantAsk,
        holder: MemberId,
        at: OffsetDateTime,
    ) -> StoreResult<()> {
        let statement = sqlx::query(
            "UPDATE grant_asks SET settled_at = ?7 \
             WHERE project_id = ?1 AND asker_id = ?2 AND holder_id = ?3 AND repo = ?4 \
             AND area = ?5 AND item_id = ?6 AND settled_at IS NULL",
        )
        .bind(int(project.get())?)
        .bind(int(asked.asker.get())?)
        .bind(int(holder.get())?)
        .bind(asked.area.repo.as_str().to_owned())
        .bind(asked.area.name.as_str().to_owned())
        .bind(int(asked.item.get())?)
        .bind(nanos(at)?);
        self.run(statement).await?;
        Ok(())
    }

    async fn settle_asked_grants_between(
        &mut self,
        project: ProjectId,
        asker: MemberId,
        holder: MemberId,
        area: &QualifiedArea,
        at: OffsetDateTime,
    ) -> StoreResult<()> {
        let statement = sqlx::query(
            "UPDATE grant_asks SET settled_at = ?6 \
             WHERE project_id = ?1 AND asker_id = ?2 AND holder_id = ?3 AND repo = ?4 \
             AND area = ?5 AND settled_at IS NULL",
        )
        .bind(int(project.get())?)
        .bind(int(asker.get())?)
        .bind(int(holder.get())?)
        .bind(area.repo.as_str().to_owned())
        .bind(area.name.as_str().to_owned())
        .bind(nanos(at)?);
        self.run(statement).await?;
        Ok(())
    }
}

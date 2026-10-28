//! The grants members asked of the holders of areas.

use async_trait::async_trait;
use sqlx::sqlite::SqliteRow;

use dibs_app::ports::{AskedGrantRows, StoreResult};
use dibs_domain::area::{AreaName, QualifiedArea};
use dibs_domain::grant::{AskedGrant, GrantAsk};
use dibs_domain::ids::{ItemId, MemberId, MessageId, ProjectId};
use dibs_domain::repo::RepoId;

use crate::SqliteTx;
use crate::codec::{Fields, int};

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
    ) -> StoreResult<Vec<AskedGrant>> {
        let statement = sqlx::query(
            "SELECT g.message_id, g.asker_id, g.item_id, g.repo, g.area, g.holder_id \
             FROM grant_asks g \
             JOIN messages asking ON asking.project_id = g.project_id AND asking.id = g.message_id \
             WHERE g.project_id = ?1 AND g.holder_id = ?2 AND NOT EXISTS (SELECT 1 FROM messages m \
                 WHERE m.project_id = g.project_id AND m.thread_id = asking.thread_id \
                 AND m.author_id = g.holder_id) \
             ORDER BY g.message_id",
        )
        .bind(int(project.get())?)
        .bind(int(holder.get())?);
        self.all(statement).await?.iter().map(asked_grant).collect()
    }
}

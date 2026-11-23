//! Grants.

use async_trait::async_trait;
use sqlx::sqlite::SqliteRow;

use dibs_app::ports::{GrantRows, StoreResult};
use dibs_app::records::GrantRecord;
use dibs_domain::area::{AreaName, QualifiedArea};
use dibs_domain::grant::{Grant, GrantBlock, GrantDecision};
use dibs_domain::ids::{GrantId, ItemId, MemberId, ProjectId};
use dibs_domain::repo::RepoId;

use crate::SqliteTx;
use crate::codec::{Fields, int, nanos};

fn grant(row: &SqliteRow) -> StoreResult<GrantRecord> {
    let f = Fields::of(row, "grants");
    Ok(GrantRecord {
        id: GrantId(f.number("id")?),
        grant: Grant {
            item: ItemId(f.number("item_id")?),
            area: QualifiedArea {
                repo: f.parse("repo", RepoId::parse)?,
                name: f.parse("area", AreaName::parse)?,
            },
            from: MemberId(f.number("from_member")?),
            to: MemberId(f.number("to_member")?),
        },
        created_at: f.time("created_at")?,
        decided: f.parse("decided", GrantDecision::parse)?,
    })
}

fn grant_block(row: &SqliteRow) -> StoreResult<GrantBlock> {
    let f = Fields::of(row, "grant_blocks");
    Ok(GrantBlock {
        asker: MemberId(f.number("asker_id")?),
        asker_item: ItemId(f.number("asker_item")?),
        asker_claimed_at: f.time("asker_claimed_at")?,
        holder: MemberId(f.number("holder_id")?),
        holder_item: ItemId(f.number("holder_item")?),
        holder_claimed_at: f.time("holder_claimed_at")?,
        area: f.parse("area", QualifiedArea::parse)?,
        revoked_at: f.time("revoked_at")?,
    })
}

#[async_trait]
impl GrantRows for SqliteTx {
    async fn grants(&mut self, project: ProjectId) -> StoreResult<Vec<GrantRecord>> {
        let statement = sqlx::query(
            "SELECT id, item_id, repo, area, from_member, to_member, created_at, decided FROM grants \
             WHERE project_id = ?1 ORDER BY id",
        )
        .bind(int(project.get())?);
        self.all(statement).await?.iter().map(grant).collect()
    }

    async fn grant(&mut self, project: ProjectId, id: GrantId) -> StoreResult<Option<GrantRecord>> {
        let statement = sqlx::query(
            "SELECT id, item_id, repo, area, from_member, to_member, created_at, decided FROM grants \
             WHERE project_id = ?1 AND id = ?2",
        )
        .bind(int(project.get())?)
        .bind(int(id.get())?);
        self.optional(statement)
            .await?
            .as_ref()
            .map(grant)
            .transpose()
    }

    async fn insert_grant(&mut self, project: ProjectId, record: &GrantRecord) -> StoreResult<()> {
        let statement = sqlx::query(
            "INSERT INTO grants (project_id, id, item_id, repo, area, from_member, to_member, \
             created_at, decided) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        )
        .bind(int(project.get())?)
        .bind(int(record.id.get())?)
        .bind(int(record.grant.item.get())?)
        .bind(record.grant.area.repo.as_str().to_owned())
        .bind(record.grant.area.name.as_str().to_owned())
        .bind(int(record.grant.from.get())?)
        .bind(int(record.grant.to.get())?)
        .bind(nanos(record.created_at)?)
        .bind(record.decided.code());
        self.run(statement).await?;
        Ok(())
    }

    async fn delete_grant(&mut self, project: ProjectId, id: GrantId) -> StoreResult<()> {
        let statement = sqlx::query("DELETE FROM grants WHERE project_id = ?1 AND id = ?2")
            .bind(int(project.get())?)
            .bind(int(id.get())?);
        self.run(statement).await?;
        Ok(())
    }

    async fn insert_grant_block(
        &mut self,
        project: ProjectId,
        block: &GrantBlock,
    ) -> StoreResult<()> {
        let statement = sqlx::query(
            "INSERT INTO grant_blocks (project_id, asker_id, asker_item, asker_claimed_at, \
             holder_id, holder_item, holder_claimed_at, area, revoked_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9) \
             ON CONFLICT (project_id, asker_id, asker_item, asker_claimed_at, holder_id, \
             holder_item, holder_claimed_at, area) DO NOTHING",
        )
        .bind(int(project.get())?)
        .bind(int(block.asker.get())?)
        .bind(int(block.asker_item.get())?)
        .bind(nanos(block.asker_claimed_at)?)
        .bind(int(block.holder.get())?)
        .bind(int(block.holder_item.get())?)
        .bind(nanos(block.holder_claimed_at)?)
        .bind(block.area.to_string())
        .bind(nanos(block.revoked_at)?);
        self.run(statement).await?;
        Ok(())
    }

    async fn grant_blocks(&mut self, project: ProjectId) -> StoreResult<Vec<GrantBlock>> {
        let statement = sqlx::query(
            "SELECT asker_id, asker_item, asker_claimed_at, holder_id, holder_item, \
             holder_claimed_at, area, revoked_at FROM grant_blocks WHERE project_id = ?1 \
             ORDER BY asker_id, asker_item, asker_claimed_at, holder_id, holder_item, \
             holder_claimed_at, area",
        )
        .bind(int(project.get())?);
        self.all(statement).await?.iter().map(grant_block).collect()
    }
}

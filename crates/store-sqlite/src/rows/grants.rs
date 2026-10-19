//! Grants.

use async_trait::async_trait;
use sqlx::sqlite::SqliteRow;

use dibs_app::ports::{GrantRows, StoreResult};
use dibs_app::records::GrantRecord;
use dibs_domain::area::{AreaName, QualifiedArea};
use dibs_domain::grant::Grant;
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
    })
}

#[async_trait]
impl GrantRows for SqliteTx {
    async fn grants(&mut self, project: ProjectId) -> StoreResult<Vec<GrantRecord>> {
        let statement = sqlx::query(
            "SELECT id, item_id, repo, area, from_member, to_member, created_at FROM grants \
             WHERE project_id = ?1 ORDER BY id",
        )
        .bind(int(project.get())?);
        self.all(statement).await?.iter().map(grant).collect()
    }

    async fn grant(&mut self, project: ProjectId, id: GrantId) -> StoreResult<Option<GrantRecord>> {
        let statement = sqlx::query(
            "SELECT id, item_id, repo, area, from_member, to_member, created_at FROM grants \
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
             created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )
        .bind(int(project.get())?)
        .bind(int(record.id.get())?)
        .bind(int(record.grant.item.get())?)
        .bind(record.grant.area.repo.as_str().to_owned())
        .bind(record.grant.area.name.as_str().to_owned())
        .bind(int(record.grant.from.get())?)
        .bind(int(record.grant.to.get())?)
        .bind(nanos(record.created_at)?);
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
}

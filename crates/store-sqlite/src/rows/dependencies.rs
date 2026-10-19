//! The work graph: which item is blocked by which.

use async_trait::async_trait;
use sqlx::sqlite::SqliteRow;

use dibs_app::ports::{DependencyRows, StoreResult};
use dibs_domain::dependency::Dependency;
use dibs_domain::ids::{ItemId, ProjectId};

use crate::SqliteTx;
use crate::codec::{Fields, int, name};

fn dependency(row: &SqliteRow) -> StoreResult<Dependency> {
    let f = Fields::of(row, "dependencies");
    Ok(Dependency {
        item: ItemId(f.number("item_id")?),
        blocked_by: ItemId(f.number("blocked_by_id")?),
        source: f.named("source")?,
    })
}

#[async_trait]
impl DependencyRows for SqliteTx {
    async fn dependencies(&mut self, project: ProjectId) -> StoreResult<Vec<Dependency>> {
        let statement = sqlx::query(
            "SELECT item_id, blocked_by_id, source FROM dependencies WHERE project_id = ?1 \
             ORDER BY item_id, blocked_by_id",
        )
        .bind(int(project.get())?);
        self.all(statement).await?.iter().map(dependency).collect()
    }

    async fn save_dependency(&mut self, project: ProjectId, edge: &Dependency) -> StoreResult<()> {
        let statement = sqlx::query(
            "INSERT INTO dependencies (project_id, item_id, blocked_by_id, source) \
             VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT (project_id, item_id, blocked_by_id) DO UPDATE SET source = excluded.source",
        )
        .bind(int(project.get())?)
        .bind(int(edge.item.get())?)
        .bind(int(edge.blocked_by.get())?)
        .bind(name(&edge.source)?);
        self.run(statement).await?;
        Ok(())
    }

    async fn delete_dependency(
        &mut self,
        project: ProjectId,
        item: ItemId,
        blocked_by: ItemId,
    ) -> StoreResult<()> {
        let statement = sqlx::query(
            "DELETE FROM dependencies WHERE project_id = ?1 AND item_id = ?2 AND blocked_by_id = ?3",
        )
        .bind(int(project.get())?)
        .bind(int(item.get())?)
        .bind(int(blocked_by.get())?);
        self.run(statement).await?;
        Ok(())
    }
}

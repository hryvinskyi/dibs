//! Projects.

use async_trait::async_trait;
use sqlx::sqlite::SqliteRow;

use dibs_app::ports::{ProjectRows, StoreResult};
use dibs_app::records::ProjectRecord;
use dibs_domain::ids::ProjectId;
use dibs_domain::project::ProjectSlug;

use crate::SqliteTx;
use crate::codec::{Fields, int, nanos};

fn project(row: &SqliteRow) -> StoreResult<ProjectRecord> {
    let f = Fields::of(row, "projects");
    Ok(ProjectRecord {
        id: ProjectId(f.number("id")?),
        slug: f.parse("slug", ProjectSlug::parse)?,
        created_at: f.time("created_at")?,
    })
}

#[async_trait]
impl ProjectRows for SqliteTx {
    async fn insert_project(&mut self, record: &ProjectRecord) -> StoreResult<()> {
        let statement =
            sqlx::query("INSERT INTO projects (id, slug, created_at) VALUES (?1, ?2, ?3)")
                .bind(int(record.id.get())?)
                .bind(record.slug.as_str().to_owned())
                .bind(nanos(record.created_at)?);
        self.run(statement).await?;
        Ok(())
    }

    async fn project(&mut self, id: ProjectId) -> StoreResult<Option<ProjectRecord>> {
        let statement = sqlx::query("SELECT id, slug, created_at FROM projects WHERE id = ?1")
            .bind(int(id.get())?);
        self.optional(statement)
            .await?
            .as_ref()
            .map(project)
            .transpose()
    }

    async fn project_by_slug(&mut self, slug: &ProjectSlug) -> StoreResult<Option<ProjectRecord>> {
        let statement = sqlx::query("SELECT id, slug, created_at FROM projects WHERE slug = ?1")
            .bind(slug.as_str().to_owned());
        self.optional(statement)
            .await?
            .as_ref()
            .map(project)
            .transpose()
    }
}

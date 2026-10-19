//! Work items.

use async_trait::async_trait;
use sqlx::sqlite::SqliteRow;

use dibs_app::ports::{ItemRows, StoreResult};
use dibs_app::records::{ItemFilter, ItemRecord};
use dibs_domain::ids::{ItemId, MemberId, ProjectId};
use dibs_domain::item::{
    ExternalRef, ItemRepository, ItemStatus, Priority, SourceId, SourceStatus, Title,
};
use dibs_domain::repo::{RemoteUrl, RepoId};

use crate::SqliteTx;
use crate::codec::{Fields, int, json, name, nanos};

/// The columns an item is read from.
macro_rules! columns {
    () => {
        "id, external_ref, title, status, repo, remote, areas_hint, priority, source_status, \
         closed, source, created_by, created_at"
    };
}

/// The repository as its two columns: a listed repository's id, or an unlisted one's remote.
fn repository_columns(repository: &ItemRepository) -> (Option<String>, Option<String>) {
    match repository {
        ItemRepository::Listed(repo) => (Some(repo.as_str().to_owned()), None),
        ItemRepository::Unlisted(remote) => (None, Some(remote.as_str().to_owned())),
        ItemRepository::Unnamed => (None, None),
    }
}

fn repository(f: &Fields<'_>) -> StoreResult<ItemRepository> {
    let repo = f.opt_parse("repo", RepoId::parse)?;
    let remote = f.opt_parse("remote", RemoteUrl::parse)?;
    match (repo, remote) {
        (Some(repo), None) => Ok(ItemRepository::Listed(repo)),
        (None, Some(remote)) => Ok(ItemRepository::Unlisted(remote)),
        (None, None) => Ok(ItemRepository::Unnamed),
        (Some(_), Some(_)) => Err(f.bad("remote", "set beside repo")),
    }
}

fn item(row: &SqliteRow) -> StoreResult<ItemRecord> {
    let f = Fields::of(row, "items");
    Ok(ItemRecord {
        id: ItemId(f.number("id")?),
        external_ref: f.opt_parse("external_ref", ExternalRef::parse)?,
        title: f.parse("title", Title::parse)?,
        status: f.named("status")?,
        repository: repository(&f)?,
        areas_hint: f.json("areas_hint")?,
        priority: {
            let number = u8::try_from(f.int("priority")?)
                .map_err(|_| f.bad("priority", "not a priority"))?;
            Priority::new(number).map_err(|error| f.bad("priority", error))?
        },
        source_status: f.opt_parse("source_status", SourceStatus::parse)?,
        closed: f.flag("closed")?,
        source: f.opt_parse("source", SourceId::parse)?,
        created_by: MemberId(f.number("created_by")?),
        created_at: f.time("created_at")?,
    })
}

#[async_trait]
impl ItemRows for SqliteTx {
    async fn insert_item(&mut self, project: ProjectId, record: &ItemRecord) -> StoreResult<()> {
        let (repo, remote) = repository_columns(&record.repository);
        let statement = sqlx::query(
            "INSERT INTO items (project_id, id, external_ref, title, status, repo, remote, \
             created_by, created_at, areas_hint, priority, source_status, closed, source) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        )
        .bind(int(project.get())?)
        .bind(int(record.id.get())?)
        .bind(record.external_ref.as_ref().map(ToString::to_string))
        .bind(record.title.as_str().to_owned())
        .bind(name(&record.status)?)
        .bind(repo)
        .bind(remote)
        .bind(int(record.created_by.get())?)
        .bind(nanos(record.created_at)?)
        .bind(json(&record.areas_hint)?)
        .bind(i64::from(record.priority.get()))
        .bind(record.source_status.as_ref().map(|s| s.as_str().to_owned()))
        .bind(i64::from(record.closed))
        .bind(record.source.as_ref().map(|s| s.as_str().to_owned()));
        self.run(statement).await?;
        Ok(())
    }

    async fn item(&mut self, project: ProjectId, id: ItemId) -> StoreResult<Option<ItemRecord>> {
        let statement = sqlx::query(concat!(
            "SELECT ",
            columns!(),
            " FROM items WHERE project_id = ?1 AND id = ?2"
        ))
        .bind(int(project.get())?)
        .bind(int(id.get())?);
        self.optional(statement)
            .await?
            .as_ref()
            .map(item)
            .transpose()
    }

    async fn item_by_ref(
        &mut self,
        project: ProjectId,
        reference: &ExternalRef,
    ) -> StoreResult<Option<ItemRecord>> {
        let statement = sqlx::query(concat!(
            "SELECT ",
            columns!(),
            " FROM items WHERE project_id = ?1 AND external_ref = ?2"
        ))
        .bind(int(project.get())?)
        .bind(reference.to_string());
        self.optional(statement)
            .await?
            .as_ref()
            .map(item)
            .transpose()
    }

    async fn items(
        &mut self,
        project: ProjectId,
        filter: &ItemFilter,
    ) -> StoreResult<Vec<ItemRecord>> {
        let status = filter.status.as_ref().map(name).transpose()?;
        let holder = filter.holder.map(|member| int(member.get())).transpose()?;
        let statement = sqlx::query(concat!(
            "SELECT ",
            columns!(),
            " FROM items i WHERE i.project_id = ?1 \
             AND (?2 IS NULL OR i.status = ?2) \
             AND (?3 IS NULL OR EXISTS (SELECT 1 FROM claims c WHERE c.project_id = i.project_id \
                  AND c.item_id = i.id AND c.member_id = ?3)) \
             AND (?4 IS NULL OR i.external_ref = ?4) \
             ORDER BY i.id"
        ))
        .bind(int(project.get())?)
        .bind(status)
        .bind(holder)
        .bind(filter.external_ref.as_ref().map(ToString::to_string));
        self.all(statement).await?.iter().map(item).collect()
    }

    async fn set_item_status(
        &mut self,
        project: ProjectId,
        id: ItemId,
        status: ItemStatus,
    ) -> StoreResult<()> {
        let statement =
            sqlx::query("UPDATE items SET status = ?3 WHERE project_id = ?1 AND id = ?2")
                .bind(int(project.get())?)
                .bind(int(id.get())?)
                .bind(name(&status)?);
        self.run(statement).await?;
        Ok(())
    }

    async fn update_item(&mut self, project: ProjectId, record: &ItemRecord) -> StoreResult<()> {
        let statement = sqlx::query(
            "UPDATE items SET title = ?3, areas_hint = ?4, priority = ?5, source_status = ?6, \
             closed = ?7, source = ?8 WHERE project_id = ?1 AND id = ?2",
        )
        .bind(int(project.get())?)
        .bind(int(record.id.get())?)
        .bind(record.title.as_str().to_owned())
        .bind(json(&record.areas_hint)?)
        .bind(i64::from(record.priority.get()))
        .bind(record.source_status.as_ref().map(|s| s.as_str().to_owned()))
        .bind(i64::from(record.closed))
        .bind(record.source.as_ref().map(|s| s.as_str().to_owned()));
        self.run(statement).await?;
        Ok(())
    }
}

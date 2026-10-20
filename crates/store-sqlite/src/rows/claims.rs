//! Claims and the paths recorded as shared under them.

use async_trait::async_trait;
use sqlx::sqlite::SqliteRow;

use dibs_app::ports::{ClaimRows, StoreResult};
use dibs_domain::claim::Claim;
use dibs_domain::ids::{ItemId, MemberId, ProjectId, SessionId};
use dibs_domain::paths::{RelPath, RepoPath};
use dibs_domain::repo::RepoId;
use dibs_domain::reservation::SharedPath;
use dibs_domain::session::HostName;
use dibs_domain::wide::{WideReason, WideScope};

use crate::SqliteTx;
use crate::codec::{Fields, int, json, nanos};

/// The columns a claim is read from.
macro_rules! claim_columns {
    () => {
        "item_id, member_id, session_id, host, areas, wide_areas, wide_reason, claimed_at, \
         renewed_at, last_seen"
    };
}

fn claim(row: &SqliteRow) -> StoreResult<Claim> {
    let f = Fields::of(row, "claims");
    let wide = match (f.opt_text("wide_areas")?, f.opt_text("wide_reason")?) {
        (None, None) => None,
        (Some(areas), Some(reason)) => {
            let areas = serde_json::from_str(&areas).map_err(|error| f.bad("wide_areas", error))?;
            let reason = WideReason::parse(&reason).map_err(|error| f.bad("wide_reason", error))?;
            Some(WideScope::new(areas, reason).map_err(|error| f.bad("wide_areas", error))?)
        }
        _ => {
            return Err(f.bad(
                "wide_areas",
                "a wide scope has both its areas and its reason or neither",
            ));
        }
    };
    Ok(Claim {
        item: ItemId(f.number("item_id")?),
        member: MemberId(f.number("member_id")?),
        session: SessionId(f.number("session_id")?),
        host: f.parse("host", HostName::parse)?,
        areas: f.json("areas")?,
        wide,
        claimed_at: f.time("claimed_at")?,
        renewed_at: f.time("renewed_at")?,
        last_seen: f.time("last_seen")?,
    })
}

fn shared_path(row: &SqliteRow) -> StoreResult<SharedPath> {
    let f = Fields::of(row, "shared_paths");
    Ok(SharedPath {
        claim: ItemId(f.number("item_id")?),
        path: RepoPath {
            repo: f.parse("repo", RepoId::parse)?,
            path: f.parse("path", RelPath::parse)?,
        },
    })
}

#[async_trait]
impl ClaimRows for SqliteTx {
    async fn claim(&mut self, project: ProjectId, item: ItemId) -> StoreResult<Option<Claim>> {
        let statement = sqlx::query(concat!(
            "SELECT ",
            claim_columns!(),
            " FROM claims WHERE project_id = ?1 AND item_id = ?2"
        ))
        .bind(int(project.get())?)
        .bind(int(item.get())?);
        self.optional(statement)
            .await?
            .as_ref()
            .map(claim)
            .transpose()
    }

    async fn claims(&mut self, project: ProjectId) -> StoreResult<Vec<Claim>> {
        let statement = sqlx::query(concat!(
            "SELECT ",
            claim_columns!(),
            " FROM claims WHERE project_id = ?1 ORDER BY item_id"
        ))
        .bind(int(project.get())?);
        self.all(statement).await?.iter().map(claim).collect()
    }

    async fn save_claim(&mut self, project: ProjectId, record: &Claim) -> StoreResult<()> {
        let statement = sqlx::query(
            "INSERT INTO claims (project_id, item_id, member_id, session_id, host, areas, \
             wide_areas, wide_reason, claimed_at, renewed_at, last_seen) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11) \
             ON CONFLICT (project_id, item_id) DO UPDATE SET member_id = excluded.member_id, \
             session_id = excluded.session_id, host = excluded.host, areas = excluded.areas, \
             wide_areas = excluded.wide_areas, wide_reason = excluded.wide_reason, \
             claimed_at = excluded.claimed_at, renewed_at = excluded.renewed_at, \
             last_seen = excluded.last_seen",
        )
        .bind(int(project.get())?)
        .bind(int(record.item.get())?)
        .bind(int(record.member.get())?)
        .bind(int(record.session.get())?)
        .bind(record.host.as_str().to_owned())
        .bind(json(&record.areas)?)
        .bind(
            record
                .wide
                .as_ref()
                .map(|scope| json(scope.areas()))
                .transpose()?,
        )
        .bind(
            record
                .wide
                .as_ref()
                .map(|scope| scope.reason().as_str().to_owned()),
        )
        .bind(nanos(record.claimed_at)?)
        .bind(nanos(record.renewed_at)?)
        .bind(nanos(record.last_seen)?);
        self.run(statement).await?;
        Ok(())
    }

    async fn delete_claim(&mut self, project: ProjectId, item: ItemId) -> StoreResult<()> {
        let statement = sqlx::query("DELETE FROM claims WHERE project_id = ?1 AND item_id = ?2")
            .bind(int(project.get())?)
            .bind(int(item.get())?);
        self.run(statement).await?;
        Ok(())
    }

    async fn shared_paths(&mut self, project: ProjectId) -> StoreResult<Vec<SharedPath>> {
        let statement = sqlx::query(
            "SELECT item_id, repo, path FROM shared_paths WHERE project_id = ?1 \
             ORDER BY item_id, repo, path",
        )
        .bind(int(project.get())?);
        self.all(statement).await?.iter().map(shared_path).collect()
    }

    async fn insert_shared_path(
        &mut self,
        project: ProjectId,
        shared: &SharedPath,
    ) -> StoreResult<()> {
        let statement = sqlx::query(
            "INSERT INTO shared_paths (project_id, item_id, repo, path) VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT (project_id, item_id, repo, path) DO NOTHING",
        )
        .bind(int(project.get())?)
        .bind(int(shared.claim.get())?)
        .bind(shared.path.repo.as_str().to_owned())
        .bind(shared.path.path.as_str().to_owned());
        self.run(statement).await?;
        Ok(())
    }

    async fn delete_shared_paths(&mut self, project: ProjectId, item: ItemId) -> StoreResult<()> {
        let statement =
            sqlx::query("DELETE FROM shared_paths WHERE project_id = ?1 AND item_id = ?2")
                .bind(int(project.get())?)
                .bind(int(item.get())?);
        self.run(statement).await?;
        Ok(())
    }
}

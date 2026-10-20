//! Claims and the paths recorded as shared under them.

use std::collections::BTreeMap;

use async_trait::async_trait;
use sqlx::sqlite::SqliteRow;

use dibs_app::ports::{ClaimRows, StoreResult};
use dibs_domain::area::QualifiedArea;
use dibs_domain::claim::Claim;
use dibs_domain::handover::{ClaimKind, PendingArea};
use dibs_domain::ids::{ItemId, MemberId, ProjectId, SessionId, ThreadId};
use dibs_domain::paths::{RelPath, RepoPath};
use dibs_domain::repo::RepoId;
use dibs_domain::reservation::SharedPath;
use dibs_domain::review::{ClaimState, Review};
use dibs_domain::session::HostName;
use dibs_domain::wide::{WideReason, WideScope};

use crate::SqliteTx;
use crate::codec::{Fields, int, json, nanos, opt_nanos};

/// The columns a claim is read from.
macro_rules! claim_columns {
    () => {
        "item_id, member_id, session_id, host, areas, wide_areas, wide_reason, claimed_at, \
         renewed_at, last_seen, review_prs, review_since, review_more, branches, kind"
    };
}

/// A claim's row, its pending areas read with [`pending_area`] and given by the caller.
fn claim(row: &SqliteRow, pending: BTreeMap<QualifiedArea, PendingArea>) -> StoreResult<Claim> {
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
    let state = match (
        f.opt_text("review_prs")?,
        f.opt_time("review_since")?,
        f.opt_int("review_more")?,
    ) {
        (None, None, None) => ClaimState::Working,
        (Some(prs), Some(since), Some(more)) => {
            let prs = serde_json::from_str(&prs).map_err(|error| f.bad("review_prs", error))?;
            let review =
                Review::new(prs, since, more != 0).map_err(|error| f.bad("review_prs", error))?;
            ClaimState::InReview(review)
        }
        _ => {
            return Err(f.bad(
                "review_prs",
                "a review has its pull requests, its start and whether more follow, or none of them",
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
        state,
        branches: f.json("branches")?,
        kind: f.parse("kind", ClaimKind::parse)?,
        pending,
    })
}

/// The columns a pending area is read from.
macro_rules! pending_columns {
    () => {
        "item_id, area, from_item_id, asked_at, escalated_at, request_thread_id"
    };
}

/// One row of `claim_pending`: the claim's item, and the area it waits for with what it waits on.
fn pending_area(row: &SqliteRow) -> StoreResult<(ItemId, QualifiedArea, PendingArea)> {
    let f = Fields::of(row, "claim_pending");
    Ok((
        ItemId(f.number("item_id")?),
        f.parse("area", QualifiedArea::parse)?,
        PendingArea {
            from: ItemId(f.number("from_item_id")?),
            asked_at: f.opt_time("asked_at")?,
            escalated_at: f.opt_time("escalated_at")?,
            request: f.opt_number("request_thread_id")?.map(ThreadId),
        },
    ))
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

impl SqliteTx {
    /// Removes the areas the item's claim waits for.
    async fn delete_pending(&mut self, project: ProjectId, item: ItemId) -> StoreResult<()> {
        let statement =
            sqlx::query("DELETE FROM claim_pending WHERE project_id = ?1 AND item_id = ?2")
                .bind(int(project.get())?)
                .bind(int(item.get())?);
        self.run(statement).await?;
        Ok(())
    }
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
        let Some(row) = self.optional(statement).await? else {
            return Ok(None);
        };
        let statement = sqlx::query(concat!(
            "SELECT ",
            pending_columns!(),
            " FROM claim_pending WHERE project_id = ?1 AND item_id = ?2 ORDER BY area"
        ))
        .bind(int(project.get())?)
        .bind(int(item.get())?);
        let mut pending = BTreeMap::new();
        for row in self.all(statement).await? {
            let (_, area, waits) = pending_area(&row)?;
            pending.insert(area, waits);
        }
        claim(&row, pending).map(Some)
    }

    async fn claims(&mut self, project: ProjectId) -> StoreResult<Vec<Claim>> {
        let statement = sqlx::query(concat!(
            "SELECT ",
            pending_columns!(),
            " FROM claim_pending WHERE project_id = ?1 ORDER BY item_id, area"
        ))
        .bind(int(project.get())?);
        let mut pending: BTreeMap<ItemId, BTreeMap<QualifiedArea, PendingArea>> = BTreeMap::new();
        for row in self.all(statement).await? {
            let (item, area, waits) = pending_area(&row)?;
            pending.entry(item).or_default().insert(area, waits);
        }
        let statement = sqlx::query(concat!(
            "SELECT ",
            claim_columns!(),
            " FROM claims WHERE project_id = ?1 ORDER BY item_id"
        ))
        .bind(int(project.get())?);
        self.all(statement)
            .await?
            .iter()
            .map(|row| {
                let item = ItemId(Fields::of(row, "claims").number("item_id")?);
                claim(row, pending.remove(&item).unwrap_or_default())
            })
            .collect()
    }

    async fn save_claim(&mut self, project: ProjectId, record: &Claim) -> StoreResult<()> {
        let review = record.state.review();
        let statement = sqlx::query(
            "INSERT INTO claims (project_id, item_id, member_id, session_id, host, areas, \
             wide_areas, wide_reason, claimed_at, renewed_at, last_seen, review_prs, review_since, \
             review_more, branches, kind) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16) \
             ON CONFLICT (project_id, item_id) DO UPDATE SET member_id = excluded.member_id, \
             session_id = excluded.session_id, host = excluded.host, areas = excluded.areas, \
             wide_areas = excluded.wide_areas, wide_reason = excluded.wide_reason, \
             claimed_at = excluded.claimed_at, renewed_at = excluded.renewed_at, \
             last_seen = excluded.last_seen, review_prs = excluded.review_prs, \
             review_since = excluded.review_since, review_more = excluded.review_more, \
             branches = excluded.branches, kind = excluded.kind",
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
        .bind(nanos(record.last_seen)?)
        .bind(review.map(|review| json(review.prs())).transpose()?)
        .bind(opt_nanos(review.map(Review::since))?)
        .bind(review.map(|review| i64::from(review.more())))
        .bind(json(&record.branches)?)
        .bind(record.kind.code());
        self.run(statement).await?;
        self.delete_pending(project, record.item).await?;
        for (area, waits) in &record.pending {
            let statement = sqlx::query(
                "INSERT INTO claim_pending (project_id, area, item_id, from_item_id, asked_at, \
                 escalated_at, request_thread_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )
            .bind(int(project.get())?)
            .bind(area.to_string())
            .bind(int(record.item.get())?)
            .bind(int(waits.from.get())?)
            .bind(opt_nanos(waits.asked_at)?)
            .bind(opt_nanos(waits.escalated_at)?)
            .bind(waits.request.map(|thread| int(thread.get())).transpose()?);
            self.run(statement).await?;
        }
        Ok(())
    }

    async fn delete_claim(&mut self, project: ProjectId, item: ItemId) -> StoreResult<()> {
        self.delete_pending(project, item).await?;
        let statement = sqlx::query("DELETE FROM claims WHERE project_id = ?1 AND item_id = ?2")
            .bind(int(project.get())?)
            .bind(int(item.get())?);
        self.run(statement).await?;
        Ok(())
    }

    async fn pending_projects(&mut self) -> StoreResult<Vec<ProjectId>> {
        let statement =
            sqlx::query("SELECT DISTINCT project_id FROM claim_pending ORDER BY project_id");
        self.all(statement)
            .await?
            .iter()
            .map(|row| {
                Ok(ProjectId(
                    Fields::of(row, "claim_pending").number("project_id")?,
                ))
            })
            .collect()
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

//! Which conversation dibs keeps with each claim's holder about that claim.

use async_trait::async_trait;
use sqlx::sqlite::SqliteRow;
use time::OffsetDateTime;

use dibs_app::ports::{ClaimThreadRows, StoreResult};
use dibs_app::records::ClaimThread;
use dibs_domain::claim::ClaimInstance;
use dibs_domain::ids::{ItemId, MemberId, ProjectId, ThreadId};

use crate::SqliteTx;
use crate::codec::{Fields, int, nanos, opt_nanos};

fn link(row: &SqliteRow) -> StoreResult<ClaimThread> {
    let f = Fields::of(row, "claim_threads");
    Ok(ClaimThread {
        thread: ThreadId(f.number("thread_id")?),
        claim: ClaimInstance {
            item: ItemId(f.number("item_id")?),
            member: MemberId(f.number("member_id")?),
            claimed_at: f.time("claimed_at")?,
        },
        base_moved_told_at: f.opt_time("base_moved_told_at")?,
    })
}

#[async_trait]
impl ClaimThreadRows for SqliteTx {
    async fn claim_thread(
        &mut self,
        project: ProjectId,
        claim: &ClaimInstance,
    ) -> StoreResult<Option<ClaimThread>> {
        let statement = sqlx::query(
            "SELECT thread_id, item_id, member_id, claimed_at, base_moved_told_at \
             FROM claim_threads \
             WHERE project_id = ?1 AND item_id = ?2 AND member_id = ?3 AND claimed_at = ?4",
        )
        .bind(int(project.get())?)
        .bind(int(claim.item.get())?)
        .bind(int(claim.member.get())?)
        .bind(nanos(claim.claimed_at)?);
        self.optional(statement)
            .await?
            .as_ref()
            .map(link)
            .transpose()
    }

    async fn insert_claim_thread(
        &mut self,
        project: ProjectId,
        record: &ClaimThread,
    ) -> StoreResult<()> {
        let statement = sqlx::query(
            "INSERT INTO claim_threads (project_id, thread_id, item_id, member_id, claimed_at, \
             base_moved_told_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )
        .bind(int(project.get())?)
        .bind(int(record.thread.get())?)
        .bind(int(record.claim.item.get())?)
        .bind(int(record.claim.member.get())?)
        .bind(nanos(record.claim.claimed_at)?)
        .bind(opt_nanos(record.base_moved_told_at)?);
        self.run(statement).await?;
        Ok(())
    }

    async fn set_base_moved_told_at(
        &mut self,
        project: ProjectId,
        thread: ThreadId,
        at: OffsetDateTime,
    ) -> StoreResult<()> {
        let statement = sqlx::query(
            "UPDATE claim_threads SET base_moved_told_at = ?3 \
             WHERE project_id = ?1 AND thread_id = ?2",
        )
        .bind(int(project.get())?)
        .bind(int(thread.get())?)
        .bind(nanos(at)?);
        self.run(statement).await?;
        Ok(())
    }
}

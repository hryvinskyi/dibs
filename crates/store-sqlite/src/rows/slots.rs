//! The holds, waiters and offers of the project's slots.

use async_trait::async_trait;
use sqlx::sqlite::SqliteRow;

use dibs_app::ports::{SlotRows, StoreResult};
use dibs_domain::ids::{ItemId, MemberId, ProjectId, RunId, SessionId, WaiterId};
use dibs_domain::session::HostName;
use dibs_domain::slot::{
    CommitSha, Liveness, Purpose, SlotBoard, SlotHold, SlotName, SlotOffer, SlotWaiter,
    WorkflowName,
};

use crate::SqliteTx;
use crate::codec::{Fields, int, nanos, opt_nanos};

fn hold(row: &SqliteRow) -> StoreResult<SlotHold> {
    let f = Fields::of(row, "slot_holds");
    let mode = f.text("liveness")?;
    let liveness = match mode.as_str() {
        "session" => Liveness::Session,
        "run" => Liveness::Run {
            run: RunId(
                f.opt_number("run_id")?
                    .ok_or_else(|| f.bad("run_id", "a run's hold names its run"))?,
            ),
        },
        "until_run" => Liveness::UntilRun {
            workflow: f
                .opt_parse("workflow", WorkflowName::parse)?
                .ok_or_else(|| f.bad("workflow", "a hold until a run names its workflow"))?,
            sha: f
                .opt_parse("sha", CommitSha::parse)?
                .ok_or_else(|| f.bad("sha", "a hold until a run names its commit"))?,
        },
        _ => return Err(f.bad("liveness", "not a liveness mode")),
    };
    Ok(SlotHold {
        slot: f.parse("slot", SlotName::parse)?,
        member: MemberId(f.number("member_id")?),
        session: SessionId(f.number("session_id")?),
        host: f.parse("host", HostName::parse)?,
        purpose: f.parse("purpose", Purpose::parse)?,
        item: f.opt_number("item_id")?.map(ItemId),
        liveness,
        taken_at: f.time("taken_at")?,
        expires_at: f.time("expires_at")?,
        last_seen: f.time("last_seen")?,
        stalled_at: f.opt_time("stalled_at")?,
    })
}

fn waiter(row: &SqliteRow) -> StoreResult<SlotWaiter> {
    let f = Fields::of(row, "slot_waiters");
    Ok(SlotWaiter {
        id: WaiterId(f.number("id")?),
        slot: f.parse("slot", SlotName::parse)?,
        member: MemberId(f.number("member_id")?),
        purpose: f.parse("purpose", Purpose::parse)?,
        since: f.time("since")?,
        last_seen: f.time("last_seen")?,
    })
}

fn offer(row: &SqliteRow) -> StoreResult<SlotOffer> {
    let f = Fields::of(row, "slot_offers");
    Ok(SlotOffer {
        slot: f.parse("slot", SlotName::parse)?,
        member: MemberId(f.number("member_id")?),
        purpose: f.parse("purpose", Purpose::parse)?,
        until: f.time("until")?,
    })
}

/// The columns a liveness is written to.
struct LivenessColumns<'a> {
    mode: &'static str,
    run: Option<i64>,
    workflow: Option<&'a str>,
    sha: Option<&'a str>,
}

fn liveness_columns(liveness: &Liveness) -> StoreResult<LivenessColumns<'_>> {
    Ok(match liveness {
        Liveness::Session => LivenessColumns {
            mode: "session",
            run: None,
            workflow: None,
            sha: None,
        },
        Liveness::Run { run } => LivenessColumns {
            mode: "run",
            run: Some(int(run.get())?),
            workflow: None,
            sha: None,
        },
        Liveness::UntilRun { workflow, sha } => LivenessColumns {
            mode: "until_run",
            run: None,
            workflow: Some(workflow.as_str()),
            sha: Some(sha.as_str()),
        },
    })
}

#[async_trait]
impl SlotRows for SqliteTx {
    async fn slot_board(&mut self, project: ProjectId) -> StoreResult<SlotBoard> {
        let project = int(project.get())?;
        let holds = sqlx::query(
            "SELECT slot, member_id, session_id, host, purpose, item_id, liveness, run_id, \
             workflow, sha, taken_at, expires_at, last_seen, stalled_at FROM slot_holds \
             WHERE project_id = ?1 ORDER BY slot",
        )
        .bind(project);
        let holds = self
            .all(holds)
            .await?
            .iter()
            .map(hold)
            .collect::<StoreResult<_>>()?;
        let waiters = sqlx::query(
            "SELECT id, slot, member_id, purpose, since, last_seen FROM slot_waiters \
             WHERE project_id = ?1 ORDER BY since, id",
        )
        .bind(project);
        let waiters = self
            .all(waiters)
            .await?
            .iter()
            .map(waiter)
            .collect::<StoreResult<_>>()?;
        let offers = sqlx::query(
            "SELECT slot, member_id, purpose, until FROM slot_offers WHERE project_id = ?1 \
             ORDER BY slot",
        )
        .bind(project);
        let offers = self
            .all(offers)
            .await?
            .iter()
            .map(offer)
            .collect::<StoreResult<_>>()?;
        Ok(SlotBoard {
            holds,
            waiters,
            offers,
        })
    }

    async fn put_slot_board(&mut self, project: ProjectId, board: &SlotBoard) -> StoreResult<()> {
        let project = int(project.get())?;
        for clear in [
            "DELETE FROM slot_holds WHERE project_id = ?1",
            "DELETE FROM slot_waiters WHERE project_id = ?1",
            "DELETE FROM slot_offers WHERE project_id = ?1",
        ] {
            self.run(sqlx::query(clear).bind(project)).await?;
        }
        for hold in &board.holds {
            let columns = liveness_columns(&hold.liveness)?;
            let item = hold.item.map(|item| int(item.get())).transpose()?;
            let statement = sqlx::query(
                "INSERT INTO slot_holds (project_id, slot, member_id, session_id, host, purpose, \
                 item_id, liveness, run_id, workflow, sha, taken_at, expires_at, last_seen, \
                 stalled_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, \
                 ?14, ?15)",
            )
            .bind(project)
            .bind(hold.slot.as_str())
            .bind(int(hold.member.get())?)
            .bind(int(hold.session.get())?)
            .bind(hold.host.as_str())
            .bind(hold.purpose.as_str())
            .bind(item)
            .bind(columns.mode)
            .bind(columns.run)
            .bind(columns.workflow)
            .bind(columns.sha)
            .bind(nanos(hold.taken_at)?)
            .bind(nanos(hold.expires_at)?)
            .bind(nanos(hold.last_seen)?)
            .bind(opt_nanos(hold.stalled_at)?);
            self.run(statement).await?;
        }
        for waiter in &board.waiters {
            let statement = sqlx::query(
                "INSERT INTO slot_waiters (project_id, id, slot, member_id, purpose, since, \
                 last_seen) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )
            .bind(project)
            .bind(int(waiter.id.get())?)
            .bind(waiter.slot.as_str())
            .bind(int(waiter.member.get())?)
            .bind(waiter.purpose.as_str())
            .bind(nanos(waiter.since)?)
            .bind(nanos(waiter.last_seen)?);
            self.run(statement).await?;
        }
        for offer in &board.offers {
            let statement = sqlx::query(
                "INSERT INTO slot_offers (project_id, slot, member_id, purpose, until) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )
            .bind(project)
            .bind(offer.slot.as_str())
            .bind(int(offer.member.get())?)
            .bind(offer.purpose.as_str())
            .bind(nanos(offer.until)?);
            self.run(statement).await?;
        }
        Ok(())
    }

    async fn busy_slot_projects(&mut self) -> StoreResult<Vec<ProjectId>> {
        let statement = sqlx::query(
            "SELECT project_id FROM slot_holds UNION SELECT project_id FROM slot_waiters \
             UNION SELECT project_id FROM slot_offers ORDER BY project_id",
        );
        self.all(statement)
            .await?
            .iter()
            .map(|row| {
                Ok(ProjectId(
                    Fields::of(row, "slot_holds").number("project_id")?,
                ))
            })
            .collect()
    }
}

//! What members' machines reported, and what integrations last said of each pull request.

use std::collections::BTreeMap;

use async_trait::async_trait;
use sqlx::sqlite::SqliteRow;
use time::OffsetDateTime;

use dibs_app::ports::{ReportRows, StoreResult};
use dibs_domain::ids::{MemberId, ProjectId};
use dibs_domain::report::{
    DayCounts, DayReport, HookDurations, HookReport, MachineId, ReportDay, Tally,
};
use dibs_domain::review::PrRef;
use dibs_domain::session::HostName;

use crate::SqliteTx;
use crate::codec::{Fields, int, json, nanos};

const TABLE: &str = "hook_reports";

fn counts(f: &Fields<'_>) -> StoreResult<DayCounts> {
    let counts: BTreeMap<Tally, u64> = f.json("counts")?;
    let durations: Vec<HookDurations> = f.json("durations")?;
    DayCounts::new(counts, durations).map_err(|error| f.bad("durations", error))
}

fn report(row: &SqliteRow) -> StoreResult<HookReport> {
    let f = Fields::of(row, TABLE);
    Ok(HookReport {
        member: MemberId(f.number("member_id")?),
        machine: f.parse("machine", MachineId::parse)?,
        host: f.parse("host", HostName::parse)?,
        report: DayReport {
            day: f.parse("day", ReportDay::parse)?,
            counts: counts(&f)?,
        },
    })
}

#[async_trait]
impl ReportRows for SqliteTx {
    async fn hook_report(
        &mut self,
        project: ProjectId,
        member: MemberId,
        machine: &MachineId,
        day: ReportDay,
    ) -> StoreResult<Option<DayCounts>> {
        let statement = sqlx::query(
            "SELECT counts, durations FROM hook_reports \
             WHERE project_id = ?1 AND day = ?2 AND member_id = ?3 AND machine = ?4",
        )
        .bind(int(project.get())?)
        .bind(day.to_string())
        .bind(int(member.get())?)
        .bind(machine.as_str());
        self.optional(statement)
            .await?
            .as_ref()
            .map(|row| counts(&Fields::of(row, TABLE)))
            .transpose()
    }

    async fn save_hook_report(
        &mut self,
        project: ProjectId,
        report: &HookReport,
    ) -> StoreResult<()> {
        let statement = sqlx::query(
            "INSERT INTO hook_reports \
             (project_id, day, member_id, machine, host, counts, durations) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
             ON CONFLICT (project_id, day, member_id, machine) \
             DO UPDATE SET host = excluded.host, counts = excluded.counts, \
             durations = excluded.durations",
        )
        .bind(int(project.get())?)
        .bind(report.report.day.to_string())
        .bind(int(report.member.get())?)
        .bind(report.machine.as_str())
        .bind(report.host.as_str())
        .bind(json(&report.report.counts.counts)?)
        .bind(json(&report.report.counts.durations)?);
        self.run(statement).await?;
        Ok(())
    }

    async fn hook_reports(
        &mut self,
        project: ProjectId,
        from: ReportDay,
        to: ReportDay,
    ) -> StoreResult<Vec<HookReport>> {
        let statement = sqlx::query(
            "SELECT day, member_id, machine, host, counts, durations FROM hook_reports \
             WHERE project_id = ?1 AND day >= ?2 AND day <= ?3 \
             ORDER BY day, member_id, machine",
        )
        .bind(int(project.get())?)
        .bind(from.to_string())
        .bind(to.to_string());
        self.all(statement).await?.iter().map(report).collect()
    }

    async fn delete_hook_reports_before(
        &mut self,
        project: ProjectId,
        day: ReportDay,
    ) -> StoreResult<u64> {
        let statement = sqlx::query("DELETE FROM hook_reports WHERE project_id = ?1 AND day < ?2")
            .bind(int(project.get())?)
            .bind(day.to_string());
        self.run(statement).await
    }

    async fn pr_mergeable(&mut self, project: ProjectId, pr: &PrRef) -> StoreResult<Option<bool>> {
        let statement =
            sqlx::query("SELECT mergeable FROM pr_states WHERE project_id = ?1 AND pr = ?2")
                .bind(int(project.get())?)
                .bind(pr.as_str());
        self.optional(statement)
            .await?
            .as_ref()
            .map(|row| Fields::of(row, "pr_states").flag("mergeable"))
            .transpose()
    }

    async fn save_pr_mergeable(
        &mut self,
        project: ProjectId,
        pr: &PrRef,
        mergeable: bool,
        at: OffsetDateTime,
    ) -> StoreResult<()> {
        let statement = sqlx::query(
            "INSERT INTO pr_states (project_id, pr, mergeable, reported_at) \
             VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT (project_id, pr) \
             DO UPDATE SET mergeable = excluded.mergeable, reported_at = excluded.reported_at",
        )
        .bind(int(project.get())?)
        .bind(pr.as_str())
        .bind(i64::from(mergeable))
        .bind(nanos(at)?);
        self.run(statement).await?;
        Ok(())
    }

    async fn delete_pr_states_before(
        &mut self,
        project: ProjectId,
        before: OffsetDateTime,
    ) -> StoreResult<u64> {
        let statement =
            sqlx::query("DELETE FROM pr_states WHERE project_id = ?1 AND reported_at < ?2")
                .bind(int(project.get())?)
                .bind(nanos(before)?);
        self.run(statement).await
    }
}

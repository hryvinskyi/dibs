//! Members.

use async_trait::async_trait;
use sqlx::sqlite::SqliteRow;
use time::OffsetDateTime;

use dibs_app::ports::{MemberRows, StoreResult};
use dibs_app::records::MemberRecord;
use dibs_domain::handle::Handle;
use dibs_domain::ids::{MemberId, ProjectId};
use dibs_domain::member::{DisplayName, ExternalLogin};

use crate::SqliteTx;
use crate::codec::{Fields, int, name, nanos, opt_nanos};

fn member(row: &SqliteRow) -> StoreResult<MemberRecord> {
    let f = Fields::of(row, "members");
    Ok(MemberRecord {
        id: MemberId(f.number("id")?),
        handle: f.parse("handle", Handle::parse)?,
        display_name: f.parse("display_name", DisplayName::parse)?,
        external_login: f.opt_parse("external_login", ExternalLogin::parse)?,
        role: f.named("role")?,
        created_at: f.time("created_at")?,
        removed_at: f.opt_time("removed_at")?,
    })
}

/// The columns of `members` a member is read from by [`member`].
macro_rules! member_columns {
    () => {
        "id, handle, display_name, external_login, role, created_at, removed_at"
    };
}

#[async_trait]
impl MemberRows for SqliteTx {
    async fn insert_member(
        &mut self,
        project: ProjectId,
        record: &MemberRecord,
    ) -> StoreResult<()> {
        let statement = sqlx::query(
            "INSERT INTO members (project_id, id, handle, display_name, external_login, role, \
             created_at, removed_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )
        .bind(int(project.get())?)
        .bind(int(record.id.get())?)
        .bind(record.handle.as_str().to_owned())
        .bind(record.display_name.as_str().to_owned())
        .bind(
            record
                .external_login
                .as_ref()
                .map(|l| l.as_str().to_owned()),
        )
        .bind(name(&record.role)?)
        .bind(nanos(record.created_at)?)
        .bind(opt_nanos(record.removed_at)?);
        self.run(statement).await?;
        Ok(())
    }

    async fn member(
        &mut self,
        project: ProjectId,
        id: MemberId,
    ) -> StoreResult<Option<MemberRecord>> {
        let statement = sqlx::query(concat!(
            "SELECT ",
            member_columns!(),
            " FROM members WHERE project_id = ?1 AND id = ?2"
        ))
        .bind(int(project.get())?)
        .bind(int(id.get())?);
        self.optional(statement)
            .await?
            .as_ref()
            .map(member)
            .transpose()
    }

    async fn member_by_handle(
        &mut self,
        project: ProjectId,
        handle: &Handle,
    ) -> StoreResult<Option<MemberRecord>> {
        let statement = sqlx::query(concat!(
            "SELECT ",
            member_columns!(),
            " FROM members WHERE project_id = ?1 AND handle = ?2"
        ))
        .bind(int(project.get())?)
        .bind(handle.as_str().to_owned());
        self.optional(statement)
            .await?
            .as_ref()
            .map(member)
            .transpose()
    }

    async fn members(&mut self, project: ProjectId) -> StoreResult<Vec<MemberRecord>> {
        let statement = sqlx::query(concat!(
            "SELECT ",
            member_columns!(),
            " FROM members WHERE project_id = ?1 ORDER BY handle"
        ))
        .bind(int(project.get())?);
        self.all(statement).await?.iter().map(member).collect()
    }

    async fn remove_member(
        &mut self,
        project: ProjectId,
        member: MemberId,
        at: OffsetDateTime,
    ) -> StoreResult<()> {
        let statement = sqlx::query(
            "UPDATE members SET removed_at = ?3 \
             WHERE project_id = ?1 AND id = ?2 AND removed_at IS NULL",
        )
        .bind(int(project.get())?)
        .bind(int(member.get())?)
        .bind(nanos(at)?);
        self.run(statement).await?;
        Ok(())
    }

    async fn last_call(
        &mut self,
        project: ProjectId,
        member: MemberId,
    ) -> StoreResult<Option<OffsetDateTime>> {
        let statement =
            sqlx::query("SELECT at FROM member_calls WHERE project_id = ?1 AND member_id = ?2")
                .bind(int(project.get())?)
                .bind(int(member.get())?);
        self.optional(statement)
            .await?
            .as_ref()
            .map(|row| Fields::of(row, "member_calls").time("at"))
            .transpose()
    }

    async fn record_call(
        &mut self,
        project: ProjectId,
        member: MemberId,
        at: OffsetDateTime,
    ) -> StoreResult<()> {
        let statement = sqlx::query(
            "INSERT INTO member_calls (project_id, member_id, at) VALUES (?1, ?2, ?3) \
             ON CONFLICT (project_id, member_id) DO UPDATE SET at = excluded.at",
        )
        .bind(int(project.get())?)
        .bind(int(member.get())?)
        .bind(nanos(at)?);
        self.run(statement).await?;
        Ok(())
    }

    async fn last_calls(
        &mut self,
        project: ProjectId,
    ) -> StoreResult<Vec<(MemberId, OffsetDateTime)>> {
        let statement = sqlx::query(
            "SELECT member_id, at FROM member_calls WHERE project_id = ?1 ORDER BY member_id",
        )
        .bind(int(project.get())?);
        self.all(statement)
            .await?
            .iter()
            .map(|row| {
                let f = Fields::of(row, "member_calls");
                Ok((MemberId(f.number("member_id")?), f.time("at")?))
            })
            .collect()
    }
}

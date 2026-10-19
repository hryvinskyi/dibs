//! Sessions of members.

use async_trait::async_trait;
use sqlx::sqlite::SqliteRow;

use dibs_app::ports::{SessionRows, StoreResult};
use dibs_app::records::SessionRecord;
use dibs_domain::ids::{MemberId, ProjectId, SessionId};
use dibs_domain::message::SessionLabel;
use dibs_domain::session::{AgentLabel, HostName};

use crate::SqliteTx;
use crate::codec::{Fields, int, nanos};

fn session(row: &SqliteRow) -> StoreResult<SessionRecord> {
    let f = Fields::of(row, "sessions");
    Ok(SessionRecord {
        id: SessionId(f.number("id")?),
        member: MemberId(f.number("member_id")?),
        host: f.parse("host", HostName::parse)?,
        label: f.parse("label", SessionLabel::parse)?,
        agent: f.parse("agent", AgentLabel::parse)?,
        push: f.flag("push")?,
        last_seen: f.time("last_seen")?,
    })
}

#[async_trait]
impl SessionRows for SqliteTx {
    async fn session(
        &mut self,
        project: ProjectId,
        id: SessionId,
    ) -> StoreResult<Option<SessionRecord>> {
        let statement = sqlx::query(
            "SELECT id, member_id, host, label, agent, push, last_seen FROM sessions \
             WHERE project_id = ?1 AND id = ?2",
        )
        .bind(int(project.get())?)
        .bind(int(id.get())?);
        self.optional(statement)
            .await?
            .as_ref()
            .map(session)
            .transpose()
    }

    async fn session_by_label(
        &mut self,
        project: ProjectId,
        member: MemberId,
        label: &SessionLabel,
    ) -> StoreResult<Option<SessionRecord>> {
        let statement = sqlx::query(
            "SELECT id, member_id, host, label, agent, push, last_seen FROM sessions \
             WHERE project_id = ?1 AND member_id = ?2 AND label = ?3",
        )
        .bind(int(project.get())?)
        .bind(int(member.get())?)
        .bind(label.as_str().to_owned());
        self.optional(statement)
            .await?
            .as_ref()
            .map(session)
            .transpose()
    }

    async fn save_session(
        &mut self,
        project: ProjectId,
        record: &SessionRecord,
    ) -> StoreResult<()> {
        let statement = sqlx::query(
            "INSERT INTO sessions (project_id, id, member_id, host, label, agent, push, last_seen) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) \
             ON CONFLICT (project_id, id) DO UPDATE SET member_id = excluded.member_id, \
             host = excluded.host, label = excluded.label, agent = excluded.agent, \
             push = excluded.push, last_seen = excluded.last_seen",
        )
        .bind(int(project.get())?)
        .bind(int(record.id.get())?)
        .bind(int(record.member.get())?)
        .bind(record.host.as_str().to_owned())
        .bind(record.label.as_str().to_owned())
        .bind(record.agent.as_str().to_owned())
        .bind(record.push)
        .bind(nanos(record.last_seen)?);
        self.run(statement).await?;
        Ok(())
    }
}

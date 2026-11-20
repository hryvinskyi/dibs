//! Sessions of members, and the checkouts they report working in.

use async_trait::async_trait;
use sqlx::sqlite::SqliteRow;
use time::OffsetDateTime;

use dibs_app::ports::{SessionRows, StoreResult};
use dibs_app::records::SessionRecord;
use dibs_domain::ids::{MemberId, ProjectId, SessionId};
use dibs_domain::message::SessionLabel;
use dibs_domain::session::{AgentLabel, CheckoutToken, HostName, SessionCheckout};

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

fn session_checkout(row: &SqliteRow) -> StoreResult<SessionCheckout> {
    let f = Fields::of(row, "session_checkouts");
    Ok(SessionCheckout {
        session: SessionId(f.number("session_id")?),
        member: MemberId(f.number("member_id")?),
        checkout: f.parse("checkout", CheckoutToken::parse)?,
        reported_at: f.time("reported_at")?,
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

    async fn save_session_checkout(
        &mut self,
        project: ProjectId,
        checkout: &SessionCheckout,
    ) -> StoreResult<()> {
        let statement = sqlx::query(
            "INSERT INTO session_checkouts (project_id, session_id, member_id, checkout, \
             reported_at) VALUES (?1, ?2, ?3, ?4, ?5) \
             ON CONFLICT (project_id, session_id, checkout) DO UPDATE SET \
             member_id = excluded.member_id, reported_at = excluded.reported_at",
        )
        .bind(int(project.get())?)
        .bind(int(checkout.session.get())?)
        .bind(int(checkout.member.get())?)
        .bind(checkout.checkout.as_str().to_owned())
        .bind(nanos(checkout.reported_at)?);
        self.run(statement).await?;
        Ok(())
    }

    async fn session_checkouts(
        &mut self,
        project: ProjectId,
        member: MemberId,
        since: OffsetDateTime,
    ) -> StoreResult<Vec<SessionCheckout>> {
        let statement = sqlx::query(
            "SELECT session_id, member_id, checkout, reported_at FROM session_checkouts \
             WHERE project_id = ?1 AND member_id = ?2 AND reported_at >= ?3 \
             ORDER BY checkout, session_id",
        )
        .bind(int(project.get())?)
        .bind(int(member.get())?)
        .bind(nanos(since)?);
        self.all(statement)
            .await?
            .iter()
            .map(session_checkout)
            .collect()
    }

    async fn forget_session_checkouts(
        &mut self,
        project: ProjectId,
        session: SessionId,
    ) -> StoreResult<()> {
        let statement =
            sqlx::query("DELETE FROM session_checkouts WHERE project_id = ?1 AND session_id = ?2")
                .bind(int(project.get())?)
                .bind(int(session.get())?);
        self.run(statement).await?;
        Ok(())
    }

    async fn delete_session_checkouts_before(
        &mut self,
        project: ProjectId,
        before: OffsetDateTime,
    ) -> StoreResult<u64> {
        let statement =
            sqlx::query("DELETE FROM session_checkouts WHERE project_id = ?1 AND reported_at < ?2")
                .bind(int(project.get())?)
                .bind(nanos(before)?);
        self.run(statement).await
    }
}

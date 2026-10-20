//! What reached each member while they worked, and the raised messages members wrote themselves.

use async_trait::async_trait;
use sqlx::sqlite::SqliteRow;
use time::OffsetDateTime;

use dibs_app::ports::{DeliveryRows, RaisedRows, StoreResult};
use dibs_domain::delivery::{Delivery, DeliverySubject};
use dibs_domain::ids::{MemberId, MessageId, ProjectId, SessionId};

use crate::SqliteTx;
use crate::codec::{Fields, int, nanos};

fn delivery(row: &SqliteRow) -> StoreResult<Delivery> {
    let f = Fields::of(row, "message_deliveries");
    Ok(Delivery {
        member: MemberId(f.number("member_id")?),
        subject: f.parse("subject", DeliverySubject::parse)?,
        session: SessionId(f.number("session_id")?),
        at: f.time("at")?,
    })
}

#[async_trait]
impl DeliveryRows for SqliteTx {
    async fn insert_delivery(
        &mut self,
        project: ProjectId,
        delivery: &Delivery,
    ) -> StoreResult<()> {
        let statement = sqlx::query(
            "INSERT INTO message_deliveries (project_id, member_id, subject, session_id, at) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )
        .bind(int(project.get())?)
        .bind(int(delivery.member.get())?)
        .bind(delivery.subject.key())
        .bind(int(delivery.session.get())?)
        .bind(nanos(delivery.at)?);
        self.run(statement).await?;
        Ok(())
    }

    async fn delivery(
        &mut self,
        project: ProjectId,
        member: MemberId,
        subject: &DeliverySubject,
    ) -> StoreResult<Option<Delivery>> {
        let statement = sqlx::query(
            "SELECT member_id, subject, session_id, at FROM message_deliveries \
             WHERE project_id = ?1 AND member_id = ?2 AND subject = ?3",
        )
        .bind(int(project.get())?)
        .bind(int(member.get())?)
        .bind(subject.key());
        self.optional(statement)
            .await?
            .as_ref()
            .map(delivery)
            .transpose()
    }

    async fn deliveries(
        &mut self,
        project: ProjectId,
        member: MemberId,
        subjects: &[DeliverySubject],
    ) -> StoreResult<Vec<Delivery>> {
        let mut found = Vec::new();
        for subject in subjects {
            if let Some(known) = self.delivery(project, member, subject).await? {
                found.push(known);
            }
        }
        Ok(found)
    }
}

#[async_trait]
impl RaisedRows for SqliteTx {
    async fn insert_raised(
        &mut self,
        project: ProjectId,
        member: MemberId,
        message: MessageId,
        at: OffsetDateTime,
    ) -> StoreResult<()> {
        let statement = sqlx::query(
            "INSERT INTO raised_messages (project_id, message_id, member_id, at) \
             VALUES (?1, ?2, ?3, ?4)",
        )
        .bind(int(project.get())?)
        .bind(int(message.get())?)
        .bind(int(member.get())?)
        .bind(nanos(at)?);
        self.run(statement).await?;
        Ok(())
    }

    async fn raised_since(
        &mut self,
        project: ProjectId,
        member: MemberId,
        since: OffsetDateTime,
    ) -> StoreResult<Vec<OffsetDateTime>> {
        let statement = sqlx::query(
            "SELECT at FROM raised_messages \
             WHERE project_id = ?1 AND member_id = ?2 AND at >= ?3 ORDER BY at",
        )
        .bind(int(project.get())?)
        .bind(int(member.get())?)
        .bind(nanos(since)?);
        self.all(statement)
            .await?
            .iter()
            .map(|row| Fields::of(row, "raised_messages").time("at"))
            .collect()
    }
}

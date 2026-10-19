//! Browser sessions.

use async_trait::async_trait;
use time::OffsetDateTime;

use dibs_app::ports::{StoreResult, UiSessionRows};
use dibs_app::records::{InProject, UiSessionRecord};
use dibs_app::token::TokenHash;
use dibs_domain::ids::{MemberId, ProjectId};

use crate::SqliteTx;
use crate::codec::{Fields, int, nanos};

#[async_trait]
impl UiSessionRows for SqliteTx {
    async fn insert_ui_session(
        &mut self,
        project: ProjectId,
        record: &UiSessionRecord,
    ) -> StoreResult<()> {
        let statement = sqlx::query(
            "INSERT INTO ui_sessions (hash, project_id, member_id, token_hash, expires_at) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )
        .bind(record.hash.as_str().to_owned())
        .bind(int(project.get())?)
        .bind(int(record.member.get())?)
        .bind(record.token.as_str().to_owned())
        .bind(nanos(record.expires_at)?);
        self.run(statement).await?;
        Ok(())
    }

    async fn ui_session(
        &mut self,
        hash: &TokenHash,
    ) -> StoreResult<Option<InProject<UiSessionRecord>>> {
        let statement = sqlx::query(
            "SELECT hash, project_id, member_id, token_hash, expires_at FROM ui_sessions \
             WHERE hash = ?1",
        )
        .bind(hash.as_str().to_owned());
        let Some(row) = self.optional(statement).await? else {
            return Ok(None);
        };
        let f = Fields::of(&row, "ui_sessions");
        Ok(Some(InProject {
            project: ProjectId(f.number("project_id")?),
            record: UiSessionRecord {
                hash: TokenHash::new(f.text("hash")?),
                member: MemberId(f.number("member_id")?),
                token: TokenHash::new(f.text("token_hash")?),
                expires_at: f.time("expires_at")?,
            },
        }))
    }

    async fn extend_ui_session(
        &mut self,
        project: ProjectId,
        hash: &TokenHash,
        expires_at: OffsetDateTime,
    ) -> StoreResult<()> {
        let statement = sqlx::query(
            "UPDATE ui_sessions SET expires_at = ?3 WHERE project_id = ?1 AND hash = ?2",
        )
        .bind(int(project.get())?)
        .bind(hash.as_str().to_owned())
        .bind(nanos(expires_at)?);
        self.run(statement).await?;
        Ok(())
    }

    async fn delete_ui_session(&mut self, project: ProjectId, hash: &TokenHash) -> StoreResult<()> {
        let statement = sqlx::query("DELETE FROM ui_sessions WHERE project_id = ?1 AND hash = ?2")
            .bind(int(project.get())?)
            .bind(hash.as_str().to_owned());
        self.run(statement).await?;
        Ok(())
    }

    async fn delete_expired_ui_sessions(&mut self, now: OffsetDateTime) -> StoreResult<u64> {
        let statement =
            sqlx::query("DELETE FROM ui_sessions WHERE expires_at < ?1").bind(nanos(now)?);
        self.run(statement).await
    }
}

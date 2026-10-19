//! API tokens.

use async_trait::async_trait;
use sqlx::sqlite::SqliteRow;
use time::OffsetDateTime;

use dibs_app::ports::{StoreResult, TokenRows};
use dibs_app::records::{InProject, TokenRecord};
use dibs_app::token::{TokenHash, TokenPrefix};
use dibs_domain::ids::{MemberId, ProjectId};

use crate::SqliteTx;
use crate::codec::{Fields, int, nanos, opt_nanos};

fn token(row: &SqliteRow) -> StoreResult<TokenRecord> {
    let f = Fields::of(row, "tokens");
    Ok(TokenRecord {
        hash: TokenHash::new(f.text("hash")?),
        prefix: f.parse("prefix", TokenPrefix::parse)?,
        member: MemberId(f.number("member_id")?),
        created_at: f.time("created_at")?,
        revoked_at: f.opt_time("revoked_at")?,
    })
}

#[async_trait]
impl TokenRows for SqliteTx {
    async fn insert_token(&mut self, project: ProjectId, record: &TokenRecord) -> StoreResult<()> {
        let statement = sqlx::query(
            "INSERT INTO tokens (hash, project_id, prefix, member_id, created_at, revoked_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )
        .bind(record.hash.as_str().to_owned())
        .bind(int(project.get())?)
        .bind(record.prefix.as_str().to_owned())
        .bind(int(record.member.get())?)
        .bind(nanos(record.created_at)?)
        .bind(opt_nanos(record.revoked_at)?);
        self.run(statement).await?;
        Ok(())
    }

    async fn token_by_hash(
        &mut self,
        hash: &TokenHash,
    ) -> StoreResult<Option<InProject<TokenRecord>>> {
        let statement = sqlx::query(
            "SELECT project_id, hash, prefix, member_id, created_at, revoked_at FROM tokens \
             WHERE hash = ?1",
        )
        .bind(hash.as_str().to_owned());
        let Some(row) = self.optional(statement).await? else {
            return Ok(None);
        };
        Ok(Some(InProject {
            project: ProjectId(Fields::of(&row, "tokens").number("project_id")?),
            record: token(&row)?,
        }))
    }

    async fn token_by_prefix(
        &mut self,
        project: ProjectId,
        prefix: &TokenPrefix,
    ) -> StoreResult<Option<TokenRecord>> {
        let statement = sqlx::query(
            "SELECT hash, prefix, member_id, created_at, revoked_at FROM tokens \
             WHERE project_id = ?1 AND prefix = ?2",
        )
        .bind(int(project.get())?)
        .bind(prefix.as_str().to_owned());
        self.optional(statement)
            .await?
            .as_ref()
            .map(token)
            .transpose()
    }

    async fn tokens_of(
        &mut self,
        project: ProjectId,
        member: MemberId,
    ) -> StoreResult<Vec<TokenRecord>> {
        let statement = sqlx::query(
            "SELECT hash, prefix, member_id, created_at, revoked_at FROM tokens \
             WHERE project_id = ?1 AND member_id = ?2 ORDER BY created_at, hash",
        )
        .bind(int(project.get())?)
        .bind(int(member.get())?);
        self.all(statement).await?.iter().map(token).collect()
    }

    async fn revoke_token(
        &mut self,
        project: ProjectId,
        hash: &TokenHash,
        at: OffsetDateTime,
    ) -> StoreResult<()> {
        let statement = sqlx::query(
            "UPDATE tokens SET revoked_at = COALESCE(revoked_at, ?3) \
             WHERE project_id = ?1 AND hash = ?2",
        )
        .bind(int(project.get())?)
        .bind(hash.as_str().to_owned())
        .bind(nanos(at)?);
        self.run(statement).await?;
        Ok(())
    }
}

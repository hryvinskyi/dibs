//! The project's settings document.

use async_trait::async_trait;

use dibs_app::ports::{SettingsRows, StoreResult};
use dibs_domain::ids::ProjectId;
use dibs_domain::settings::SettingsDocument;

use crate::SqliteTx;
use crate::codec::{Fields, int, json};

#[async_trait]
impl SettingsRows for SqliteTx {
    async fn settings(&mut self, project: ProjectId) -> StoreResult<Option<SettingsDocument>> {
        let statement = sqlx::query("SELECT document FROM settings WHERE project_id = ?1")
            .bind(int(project.get())?);
        self.optional(statement)
            .await?
            .map(|row| Fields::of(&row, "settings").json("document"))
            .transpose()
    }

    async fn put_settings(
        &mut self,
        project: ProjectId,
        document: &SettingsDocument,
    ) -> StoreResult<()> {
        let statement = sqlx::query(
            "INSERT INTO settings (project_id, document) VALUES (?1, ?2) \
             ON CONFLICT (project_id) DO UPDATE SET document = excluded.document",
        )
        .bind(int(project.get())?)
        .bind(json(document)?);
        self.run(statement).await?;
        Ok(())
    }
}

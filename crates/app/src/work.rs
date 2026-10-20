//! One use case's transaction inside one project: the events it appends go into the same
//! transaction as its writes, and followers are woken only once the whole of it committed.

use std::ops::{Deref, DerefMut};

use time::OffsetDateTime;

use dibs_domain::event::EventKind;
use dibs_domain::hot_rule::HotRuleRegistry;
use dibs_domain::ids::{EventId, MemberId, ProjectId};
use dibs_domain::settings::ProjectSettings;

use crate::actor::Actor;
use crate::authority::AdminAuthority;
use crate::error::AppError;
use crate::ports::{EventNotifier, Store, Tx};

/// A transaction scoped to one project that remembers the newest event it appended.
pub(crate) struct Work {
    tx: Box<dyn Tx>,
    project: ProjectId,
    last_event: Option<EventId>,
}

impl Work {
    /// Opens a transaction for a member's request, in their project. A member removed since their
    /// request was signed in is refused as signed out: whatever they ask is decided in the same
    /// order as their removal, before it or not at all, so nothing is ever done in a removed
    /// member's name.
    ///
    /// # Errors
    /// [`AppError::Unauthorized`] when the member is no longer in the project;
    /// [`AppError::Store`] when the store fails.
    pub(crate) async fn begin_for(store: &dyn Store, actor: &Actor) -> Result<Self, AppError> {
        Self::begin_present(store, actor.project, Some(actor.member)).await
    }

    /// Opens a transaction for whoever administers a project: an admin, who must still be in it
    /// as [`Work::begin_for`] requires, or the operator of the service.
    ///
    /// # Errors
    /// [`AppError::Forbidden`] unless `by` may administer a project; [`AppError::Unauthorized`]
    /// when the admin is no longer in it; [`AppError::Store`] when the store fails.
    pub(crate) async fn begin_administering(
        store: &dyn Store,
        by: &impl AdminAuthority,
    ) -> Result<Self, AppError> {
        let project = by.administered_project()?;
        Self::begin_present(store, project, by.acting_member()).await
    }

    /// Opens a transaction the service starts for nobody: creating a project, a sweep, the
    /// retention.
    ///
    /// # Errors
    /// [`AppError::Store`] when the store fails.
    pub(crate) async fn begin_unattended(
        store: &dyn Store,
        project: ProjectId,
    ) -> Result<Self, AppError> {
        Self::begin_present(store, project, None).await
    }

    async fn begin_present(
        store: &dyn Store,
        project: ProjectId,
        member: Option<MemberId>,
    ) -> Result<Self, AppError> {
        let mut work = Self {
            tx: store.begin().await?,
            project,
            last_event: None,
        };
        if let Some(member) = member {
            let present = work
                .tx
                .member(project, member)
                .await?
                .is_some_and(|record| record.is_present());
            if !present {
                return Err(AppError::Unauthorized);
            }
        }
        Ok(work)
    }

    /// Scopes a transaction opened before its project was known (while authenticating) to that
    /// project.
    pub(crate) fn adopt(tx: Box<dyn Tx>, project: ProjectId) -> Self {
        Self {
            tx,
            project,
            last_event: None,
        }
    }

    /// The project the transaction acts in.
    pub(crate) fn project_id(&self) -> ProjectId {
        self.project
    }

    /// Appends an event to the project's log inside the transaction.
    pub(crate) async fn record(
        &mut self,
        at: OffsetDateTime,
        kind: EventKind,
    ) -> Result<EventId, AppError> {
        let id = self.tx.append_event(self.project, at, &kind).await?;
        self.last_event = Some(id);
        Ok(id)
    }

    /// The project's settings: the stored document, or the defaults when none was ever applied.
    pub(crate) async fn project_settings(
        &mut self,
        hot_rules: &HotRuleRegistry,
    ) -> Result<ProjectSettings, AppError> {
        let document = self.tx.settings(self.project).await?.unwrap_or_default();
        ProjectSettings::parse(&document, hot_rules).map_err(AppError::StoredSettings)
    }

    /// Commits a transaction that changes nothing the event log records (members, tokens, sessions,
    /// read marks, heartbeats), so there is nobody to wake.
    pub(crate) async fn commit_quietly(self) -> Result<(), AppError> {
        self.tx.commit().await?;
        Ok(())
    }

    /// Commits, then wakes the project's followers when events were appended.
    pub(crate) async fn commit(self, notifier: &dyn EventNotifier) -> Result<(), AppError> {
        self.tx.commit().await?;
        if let Some(last) = self.last_event {
            notifier.notify(self.project, last);
        }
        Ok(())
    }
}

impl Deref for Work {
    type Target = dyn Tx;

    fn deref(&self) -> &Self::Target {
        &*self.tx
    }
}

impl DerefMut for Work {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut *self.tx
    }
}

//! Replacing a project's settings.

use std::sync::Arc;

use dibs_domain::event::{EventKind, SettingsChanged};
use dibs_domain::hot_rule::HotRuleRegistry;
use dibs_domain::settings::{ProjectSettings, SettingsDocument};
use dibs_domain::slot::decide_slots_removed;

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::ports::{Clock, EventNotifier, Store};
use crate::work::Work;

/// The settings now in force.
#[derive(Debug, Clone)]
pub struct AppliedSettings {
    /// The settings.
    pub settings: ProjectSettings,
    /// Whether they differ from the ones before.
    pub changed: bool,
}

/// Validates a settings document — one repository's areas and rules at the top level, or several
/// repositories under `repos` — and makes it the project's settings. The document is stored in its
/// normal form, so applying the same settings again changes nothing and appends no event. A slot
/// the new settings no longer declare loses its hold, its queue and its offer in the same
/// transaction, each recorded as ended by the admin.
pub struct ApplySettings {
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    notifier: Arc<dyn EventNotifier>,
    hot_rules: Arc<HotRuleRegistry>,
}

impl ApplySettings {
    /// Builds the use case.
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            store: deps.store.clone(),
            clock: deps.clock.clone(),
            notifier: deps.notifier.clone(),
            hot_rules: deps.hot_rules.clone(),
        }
    }

    /// Applies the document.
    ///
    /// # Errors
    /// [`AppError::Forbidden`] unless the caller is an admin; [`AppError::Settings`] naming the
    /// first setting at fault; [`AppError::Store`] when the store fails.
    pub async fn execute(
        &self,
        actor: &Actor,
        document: SettingsDocument,
    ) -> Result<AppliedSettings, AppError> {
        actor.require_admin()?;
        let settings = ProjectSettings::parse(&document, &self.hot_rules)?;
        let normal = settings.to_document();
        let mut work = Work::begin_for(&*self.store, actor).await?;
        let before = work.settings(actor.project).await?;
        let changed = before.as_ref() != Some(&normal);
        if changed {
            let now = self.clock.now();
            work.put_settings(actor.project, &normal).await?;
            work.record(
                now,
                EventKind::SettingsChanged(SettingsChanged { by: actor.member }),
            )
            .await?;
            let board = work.slot_board(actor.project).await?;
            let removed = decide_slots_removed(&board, settings.slots(), actor.member, now);
            if removed.board != board {
                work.put_slot_board(actor.project, &removed.board).await?;
            }
            for event in removed.events {
                work.record(now, event).await?;
            }
        }
        work.commit(&*self.notifier).await?;
        Ok(AppliedSettings { settings, changed })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::use_cases::testing::{Env, SETTINGS, document};

    #[tokio::test]
    async fn applying_the_same_settings_twice_changes_them_once() {
        let env = Env::new().await;
        let changes = |kinds: &[EventKind]| {
            kinds
                .iter()
                .filter(|k| matches!(k, EventKind::SettingsChanged(_)))
                .count()
        };
        assert_eq!(changes(&env.kinds_after(0).await), 1);
        let calls = env.fakes.notifier.calls().len();
        let applied = ApplySettings::new(&env.deps)
            .execute(&env.alice.actor, document(SETTINGS))
            .await
            .unwrap();
        assert!(!applied.changed);
        assert_eq!(changes(&env.kinds_after(0).await), 1);
        assert_eq!(env.fakes.notifier.calls().len(), calls);
    }

    #[tokio::test]
    async fn a_change_is_recorded_and_announced() {
        let env = Env::new().await;
        let before = env.events().await.len();
        let applied = ApplySettings::new(&env.deps)
            .execute(
                &env.alice.actor,
                document(r#"{"areas":[{"name":"billing","paths":["billing/**"]}]}"#),
            )
            .await
            .unwrap();
        assert!(applied.changed);
        let kinds = env.kinds_after(before).await;
        assert_eq!(
            kinds,
            [EventKind::SettingsChanged(SettingsChanged {
                by: env.alice.actor.member
            })]
        );
        let last = env.events().await.last().unwrap().id;
        assert_eq!(
            env.fakes.notifier.calls().last(),
            Some(&(env.project, last))
        );
    }

    #[tokio::test]
    async fn invalid_settings_and_non_admins_are_refused() {
        let env = Env::new().await;
        let apply = ApplySettings::new(&env.deps);
        let invalid = apply
            .execute(
                &env.alice.actor,
                document(r#"{"hot":[{"paths":["a/**"],"kind":"owner","area":"nowhere"}]}"#),
            )
            .await
            .unwrap_err();
        assert_eq!(invalid.code(), "invalid_settings");
        let forbidden = apply
            .execute(&env.bob.actor, document(SETTINGS))
            .await
            .unwrap_err();
        assert_eq!(forbidden.code(), "forbidden");
    }

    #[tokio::test]
    async fn settings_with_repositories_are_kept_per_repository() {
        let env = Env::new().await;
        let applied = ApplySettings::new(&env.deps)
            .execute(
                &env.alice.actor,
                document(
                    r#"{"repos":[
                        {"id":"api","remote":"https://example.com/acme/api.git",
                         "areas":[{"name":"billing","paths":["src/billing/**"]}]},
                        {"id":"web","remote":"git@example.com:acme/web.git",
                         "areas":[{"name":"billing","paths":["app/billing/**"]}]}
                    ]}"#,
                ),
            )
            .await
            .unwrap();
        assert!(applied.settings.lists_repositories());
        let remotes: Vec<String> = applied
            .settings
            .repos()
            .iter()
            .map(|r| r.remote.as_ref().unwrap().to_string())
            .collect();
        assert_eq!(remotes, ["example.com/acme/api", "example.com/acme/web"]);
        let me = crate::use_cases::Me::new(&env.deps)
            .execute(&env.bob.actor)
            .await
            .unwrap();
        assert_eq!(me.settings, applied.settings);
    }
}

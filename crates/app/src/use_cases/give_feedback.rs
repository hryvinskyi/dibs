//! Feedback from a member's agent on the guard: a refusal it found wrong, or a conflict it resolved by
//! hand while rebasing.

use std::sync::Arc;

use dibs_domain::edit::EditCheckRefused;
use dibs_domain::event::{EventKind, FeedbackGiven};
use dibs_domain::hot_rule::HotRuleRegistry;
use dibs_domain::paths::RepoPath;
use dibs_domain::report::{Feedback, FeedbackReason, RefusalId, Tally};

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::ports::{Clock, EventNotifier, Store};
use crate::work::Work;

/// What feedback is about, as the member's agent names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeedbackAbout {
    /// A refusal the agent found wrong.
    FalseRefusal {
        /// The id the refusal was shown with.
        refusal: RefusalId,
        /// Its kind, when the agent's machine still remembers it.
        tally: Option<Tally>,
    },
    /// A file whose rebase conflict the agent resolved by hand.
    Conflict {
        /// The file.
        path: RepoPath,
    },
}

/// Feedback to record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedbackInput {
    /// What it is about.
    pub about: FeedbackAbout,
    /// Why, as the member writes it.
    pub reason: FeedbackReason,
}

/// Records feedback as one `FeedbackGiven` event, which only the member and the project's admins
/// see. A conflict is kept as its repository and the area its file lies in; the file's path is
/// not kept.
pub struct GiveFeedback {
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    notifier: Arc<dyn EventNotifier>,
    hot_rules: Arc<HotRuleRegistry>,
}

impl GiveFeedback {
    /// Builds the use case.
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            store: deps.store.clone(),
            clock: deps.clock.clone(),
            notifier: deps.notifier.clone(),
            hot_rules: deps.hot_rules.clone(),
        }
    }

    /// Records the feedback and answers it as it was kept.
    ///
    /// # Errors
    /// [`AppError::Forbidden`] for an integration; [`AppError::EditCheck`] for a conflict in a
    /// repository the project does not list; [`AppError::Store`] when the store fails.
    pub async fn execute(&self, actor: &Actor, input: FeedbackInput) -> Result<Feedback, AppError> {
        actor.require_person()?;
        let now = self.clock.now();
        let mut work = Work::begin_for(&*self.store, actor).await?;
        let about = match input.about {
            FeedbackAbout::FalseRefusal { refusal, tally } => {
                Feedback::FalseRefusal { refusal, tally }
            }
            FeedbackAbout::Conflict { path } => {
                let settings = work.project_settings(&self.hot_rules).await?;
                let spec = settings.repo(&path.repo).ok_or_else(|| {
                    AppError::EditCheck(EditCheckRefused::UnknownRepo {
                        repo: path.repo.clone(),
                    })
                })?;
                let area = spec
                    .area_for(&path.path)
                    .map(|area| spec.qualify(&area.name));
                Feedback::Conflict {
                    repo: path.repo,
                    area,
                }
            }
        };
        work.record(
            now,
            EventKind::FeedbackGiven(FeedbackGiven {
                member: actor.member,
                about: about.clone(),
                reason: input.reason,
            }),
        )
        .await?;
        work.commit(&*self.notifier).await?;
        Ok(about)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::use_cases::testing::Env;

    fn reason(text: &str) -> FeedbackReason {
        FeedbackReason::parse(text).unwrap()
    }

    #[tokio::test]
    async fn feedback_is_one_event_and_a_conflict_keeps_its_area_not_its_path() {
        let env = Env::new().await;
        let give = GiveFeedback::new(&env.deps);
        let before = env.events().await.len();
        let refusal = give
            .execute(
                &env.bob.actor,
                FeedbackInput {
                    about: FeedbackAbout::FalseRefusal {
                        refusal: RefusalId::parse("r-7f3a").unwrap(),
                        tally: Some(Tally::HeldBy),
                    },
                    reason: reason("alice released billing an hour ago"),
                },
            )
            .await
            .unwrap();
        assert!(matches!(
            refusal,
            Feedback::FalseRefusal {
                tally: Some(Tally::HeldBy),
                ..
            }
        ));
        let conflict = give
            .execute(
                &env.bob.actor,
                FeedbackInput {
                    about: FeedbackAbout::Conflict {
                        path: RepoPath::parse("default:packages/billing/secret-plan.rs").unwrap(),
                    },
                    reason: reason("both changed the rounding"),
                },
            )
            .await
            .unwrap();
        assert_eq!(
            conflict,
            Feedback::Conflict {
                repo: dibs_domain::repo::RepoId::implicit(),
                area: Some(dibs_domain::area::QualifiedArea::parse("default:billing").unwrap()),
            }
        );
        let kinds = env.kinds_after(before).await;
        assert_eq!(kinds.len(), 2);
        let text = serde_json::to_string(&kinds).unwrap();
        assert!(!text.contains("secret-plan"), "{text}");
        assert!(
            matches!(&kinds[0], EventKind::FeedbackGiven(given) if given.member == env.bob.actor.member)
        );
    }

    #[tokio::test]
    async fn a_conflict_in_an_unlisted_repository_and_an_integration_are_refused() {
        let env = Env::new().await;
        let give = GiveFeedback::new(&env.deps);
        let conflict = |path: &str| FeedbackInput {
            about: FeedbackAbout::Conflict {
                path: RepoPath::parse(path).unwrap(),
            },
            reason: reason("merged by hand"),
        };
        let error = give
            .execute(&env.bob.actor, conflict("elsewhere:a.rs"))
            .await
            .unwrap_err();
        assert_eq!(error.code(), "unknown_repo");
        let error = give
            .execute(&env.importer, conflict("default:a.rs"))
            .await
            .unwrap_err();
        assert_eq!(error.code(), "forbidden");
        let unarea = give
            .execute(&env.alice.actor, conflict("default:README.md"))
            .await
            .unwrap();
        assert!(matches!(unarea, Feedback::Conflict { area: None, .. }));
    }
}

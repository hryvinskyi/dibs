//! Asking, for each path a member is about to change, whether they may.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use dibs_domain::area::QualifiedArea;
use dibs_domain::edit::{self, EditCheckInput as DomainInput, edit_verdicts};
use dibs_domain::event::{EventKind, Reserved};
use dibs_domain::grant::Grant;
use dibs_domain::hot_rule::HotRuleRegistry;
use dibs_domain::ids::{ItemId, SessionId};
use dibs_domain::paths::RepoPath;
use dibs_domain::reservation::ReservationPlan;

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::named::{NamedVerdict, Names};
use crate::ports::{Clock, EventNotifier, Store};
use crate::use_cases::common::own_session;
use crate::work::Work;

/// The paths to check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditCheckInput {
    /// The caller's session asking.
    pub session: SessionId,
    /// The paths, each qualified by its repository.
    pub paths: Vec<RepoPath>,
    /// Whether the edits are being made: then what the check plans is recorded and the claims the
    /// edits were attributed to are refreshed. A check that only asks (`false`) writes nothing.
    pub reserve: bool,
}

/// The verdicts of a check, each with the handles of the members it names, and what the check
/// planned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditVerdicts {
    /// One verdict per path, in the order asked.
    pub verdicts: Vec<NamedVerdict>,
    /// What the check planned to record; recorded only when the edits are being made.
    pub plan: ReservationPlan,
    /// The caller's claims the edits were attributed to.
    pub touch: BTreeSet<ItemId>,
}

impl EditVerdicts {
    fn named(check: edit::EditCheck, names: &Names) -> Result<Self, AppError> {
        Ok(Self {
            verdicts: check
                .verdicts
                .into_iter()
                .map(|verdict| names.verdict(verdict))
                .collect::<Result<_, _>>()?,
            plan: check.plan,
            touch: check.touch,
        })
    }
}

/// Answers a verdict per path. When the edits are being made, it records what the domain planned —
/// areas added to the caller's claims, paths recorded as shared — and refreshes the claims the edits
/// were attributed to, wherever those claims were taken. A `Reserved` event is appended per claim
/// that actually gained something, so a repeated check appends nothing.
pub struct EditCheck {
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    notifier: Arc<dyn EventNotifier>,
    hot_rules: Arc<HotRuleRegistry>,
}

/// What one claim gains from a check.
#[derive(Default)]
struct Gain {
    areas: BTreeSet<QualifiedArea>,
    paths: BTreeSet<RepoPath>,
}

impl EditCheck {
    /// Builds the use case.
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            store: deps.store.clone(),
            clock: deps.clock.clone(),
            notifier: deps.notifier.clone(),
            hot_rules: deps.hot_rules.clone(),
        }
    }

    /// Checks the paths.
    ///
    /// # Errors
    /// [`AppError::Forbidden`] for an integration; [`AppError::NotFound`] for a session not the caller's; [`AppError::EditCheck`] for a path in a
    /// repository the project does not list; [`AppError::Store`] when the store fails.
    pub async fn execute(
        &self,
        actor: &Actor,
        input: EditCheckInput,
    ) -> Result<EditVerdicts, AppError> {
        actor.require_person()?;
        let now = self.clock.now();
        let project = actor.project;
        let mut work = Work::begin(&*self.store, project).await?;
        own_session(&mut work, actor, input.session).await?;
        let settings = work.project_settings(&self.hot_rules).await?;
        let claims = work.claims(project).await?;
        let grants: Vec<Grant> = work
            .grants(project)
            .await?
            .into_iter()
            .map(|record| record.grant)
            .collect();
        let shared = work.shared_paths(project).await?;
        let check = edit_verdicts(
            &input.paths,
            &DomainInput {
                me: actor.member,
                claims: &claims,
                grants: &grants,
                shared: &shared,
                settings: &settings,
                now,
            },
        )?;
        let names = Names::load(&mut work).await?;
        if !input.reserve {
            return EditVerdicts::named(check, &names);
        }
        let mut gains: BTreeMap<ItemId, Gain> = BTreeMap::new();
        for add in &check.plan.add_areas {
            gains
                .entry(add.claim)
                .or_default()
                .areas
                .insert(add.area.clone());
        }
        for share in &check.plan.share_paths {
            work.insert_shared_path(project, share).await?;
            gains
                .entry(share.claim)
                .or_default()
                .paths
                .insert(share.path.clone());
        }
        let changed = claims.into_iter().filter(|claim| {
            claim.member == actor.member
                && (gains.contains_key(&claim.item) || check.touch.contains(&claim.item))
        });
        for mut claim in changed {
            if let Some(gain) = gains.get(&claim.item) {
                claim.areas.extend(gain.areas.iter().cloned());
            }
            if check.touch.contains(&claim.item) {
                claim.last_seen = now;
            }
            work.save_claim(project, &claim).await?;
        }
        for (item, gain) in gains {
            work.record(
                now,
                EventKind::Reserved(Reserved {
                    item,
                    member: actor.member,
                    areas: gain.areas,
                    paths: gain.paths,
                }),
            )
            .await?;
        }
        work.commit(&*self.notifier).await?;
        EditVerdicts::named(check, &names)
    }
}

#[cfg(test)]
mod tests {
    use time::Duration;

    use dibs_domain::edit::{HeldReason, Verdict};
    use dibs_domain::handle::Handle;

    use super::*;
    use crate::use_cases::testing::{Env, Person};
    use crate::use_cases::{ApplySettings, ClaimItem, ClaimItemInput, Grant, GrantInput};

    fn path(text: &str) -> RepoPath {
        RepoPath::parse(text).unwrap()
    }

    fn area(text: &str) -> QualifiedArea {
        QualifiedArea::parse(text).unwrap()
    }

    async fn claim(env: &Env, who: &Person, item: ItemId, areas: &[&str]) {
        ClaimItem::new(&env.deps)
            .execute(
                &who.actor,
                ClaimItemInput {
                    item,
                    session: who.session,
                    areas: areas.iter().map(|a| (*a).to_owned()).collect(),
                },
            )
            .await
            .unwrap();
    }

    async fn check(env: &Env, who: &Person, paths: &[&str], reserve: bool) -> EditVerdicts {
        EditCheck::new(&env.deps)
            .execute(
                &who.actor,
                EditCheckInput {
                    session: who.session,
                    paths: paths.iter().map(|p| path(p)).collect(),
                    reserve,
                },
            )
            .await
            .unwrap()
    }

    fn verdicts(check: &EditVerdicts) -> Vec<Verdict> {
        check
            .verdicts
            .iter()
            .map(|v| v.verdict.verdict.clone())
            .collect()
    }

    #[tokio::test]
    async fn without_a_claim_a_governed_path_needs_one() {
        let env = Env::new().await;
        let result = check(
            &env,
            &env.bob,
            &["default:packages/billing/src/a.rs", "default:README.md"],
            true,
        )
        .await;
        assert_eq!(verdicts(&result), [Verdict::NeedsClaim, Verdict::Allowed]);
        assert!(result.plan.is_empty());
    }

    #[tokio::test]
    async fn an_edit_in_a_free_area_adds_it_to_the_claim_once() {
        let env = Env::new().await;
        let item = env.item("gh:acme/widgets#1").await;
        claim(&env, &env.bob, item, &[]).await;
        let before = env.events().await.len();
        let first = check(&env, &env.bob, &["default:packages/billing/a.rs"], true).await;
        assert_eq!(verdicts(&first), [Verdict::Allowed]);
        assert_eq!(
            env.kinds_after(before).await,
            [EventKind::Reserved(Reserved {
                item,
                member: env.bob.actor.member,
                areas: [area("default:billing")].into(),
                paths: BTreeSet::new(),
            })]
        );
        let mut tx = env.deps.store.begin().await.unwrap();
        let stored = tx.claim(env.project, item).await.unwrap().unwrap();
        assert!(stored.holds(&area("default:billing")));
        drop(tx);
        let after = env.events().await.len();
        let again = check(&env, &env.bob, &["default:packages/billing/b.rs"], true).await;
        assert!(again.plan.is_empty());
        assert_eq!(env.events().await.len(), after);
    }

    #[tokio::test]
    async fn asking_without_reserving_writes_nothing() {
        let env = Env::new().await;
        let item = env.item("gh:acme/widgets#1").await;
        claim(&env, &env.bob, item, &[]).await;
        env.advance(Duration::hours(1));
        let before = env.events().await.len();
        let result = check(&env, &env.bob, &["default:packages/billing/a.rs"], false).await;
        assert!(!result.plan.is_empty());
        assert_eq!(env.events().await.len(), before);
        let mut tx = env.deps.store.begin().await.unwrap();
        let stored = tx.claim(env.project, item).await.unwrap().unwrap();
        assert!(stored.areas.is_empty());
        assert_eq!(stored.last_seen, crate::use_cases::testing::START);
    }

    #[tokio::test]
    async fn an_edit_refreshes_a_claim_taken_on_another_machine() {
        let env = Env::new().await;
        let item = env.item("gh:acme/widgets#1").await;
        let laptop = crate::use_cases::testing::session(
            &env.deps,
            &env.bob.actor,
            "laptop-2",
            "laptop-2/aa",
        )
        .await;
        ClaimItem::new(&env.deps)
            .execute(
                &env.bob.actor,
                ClaimItemInput {
                    item,
                    session: laptop,
                    areas: vec!["billing".to_owned()],
                },
            )
            .await
            .unwrap();
        env.advance(Duration::hours(2));
        let before = env.events().await.len();
        let result = check(&env, &env.bob, &["default:packages/billing/a.rs"], true).await;
        assert_eq!(result.touch, [item].into());
        assert_eq!(env.events().await.len(), before);
        let mut tx = env.deps.store.begin().await.unwrap();
        let stored = tx.claim(env.project, item).await.unwrap().unwrap();
        assert_eq!(
            stored.last_seen,
            crate::use_cases::testing::START + Duration::hours(2)
        );
        assert_eq!(stored.host.as_str(), "laptop-2");
    }

    #[tokio::test]
    async fn another_members_area_is_held_until_they_grant_it() {
        let env = Env::new().await;
        let alices = env.item("gh:acme/widgets#1").await;
        let bobs = env.item("gh:acme/widgets#2").await;
        claim(&env, &env.alice, alices, &["billing"]).await;
        claim(&env, &env.bob, bobs, &[]).await;
        let held = check(&env, &env.bob, &["default:packages/billing/a.rs"], true).await;
        assert_eq!(
            verdicts(&held),
            [Verdict::HeldBy {
                member: env.alice.actor.member,
                item: alices,
                area: area("default:billing"),
                reason: HeldReason::AskHolder,
            }]
        );
        let alice = Some(Handle::parse("alice").unwrap());
        assert_eq!(held.verdicts[0].member, alice);
        Grant::new(&env.deps)
            .execute(
                &env.alice.actor,
                GrantInput {
                    item: bobs,
                    area: "billing".to_owned(),
                    to: Handle::parse("bob").unwrap(),
                },
            )
            .await
            .unwrap();
        let granted = check(&env, &env.bob, &["default:packages/billing/a.rs"], true).await;
        assert_eq!(
            verdicts(&granted),
            [Verdict::Granted {
                by: env.alice.actor.member,
                area: area("default:billing"),
            }]
        );
        assert_eq!(granted.verdicts[0].member, alice);
    }

    #[tokio::test]
    async fn a_shared_file_is_recorded_once_and_named_to_the_next_editor() {
        let env = Env::new().await;
        let alices = env.item("gh:acme/widgets#1").await;
        let bobs = env.item("gh:acme/widgets#2").await;
        claim(&env, &env.alice, alices, &[]).await;
        claim(&env, &env.bob, bobs, &[]).await;
        let before = env.events().await.len();
        let first = check(&env, &env.alice, &["default:config/app.yaml"], true).await;
        assert_eq!(verdicts(&first), [Verdict::Allowed]);
        assert_eq!(
            env.kinds_after(before).await,
            [EventKind::Reserved(Reserved {
                item: alices,
                member: env.alice.actor.member,
                areas: BTreeSet::new(),
                paths: [path("default:config/app.yaml")].into(),
            })]
        );
        let after = env.events().await.len();
        check(&env, &env.alice, &["default:config/app.yaml"], true).await;
        assert_eq!(env.events().await.len(), after);
        let second = check(&env, &env.bob, &["default:config/app.yaml"], true).await;
        let found = verdicts(&second);
        let [Verdict::AllowedWithNote { touched_by, .. }] = found.as_slice() else {
            panic!("expected a note, got {:?}", second.verdicts);
        };
        assert_eq!(touched_by[0].member, env.alice.actor.member);
        assert_eq!(
            second.verdicts[0].touched_by,
            [Handle::parse("alice").unwrap()]
        );
        assert_eq!(second.verdicts[0].member, None);
    }

    #[tokio::test]
    async fn a_path_in_an_unlisted_repository_refuses_the_check() {
        let env = Env::new().await;
        let error = EditCheck::new(&env.deps)
            .execute(
                &env.bob.actor,
                EditCheckInput {
                    session: env.bob.session,
                    paths: vec![path("web:src/a.rs")],
                    reserve: true,
                },
            )
            .await
            .unwrap_err();
        assert_eq!(error.code(), "unknown_repo");
        let session = EditCheck::new(&env.deps)
            .execute(
                &env.bob.actor,
                EditCheckInput {
                    session: env.alice.session,
                    paths: Vec::new(),
                    reserve: false,
                },
            )
            .await
            .unwrap_err();
        assert_eq!(session.code(), "not_found");
    }

    #[tokio::test]
    async fn under_the_advise_policy_a_free_governed_path_is_allowed_without_a_claim() {
        let env = Env::new().await;
        ApplySettings::new(&env.deps)
            .execute(
                &env.alice.actor,
                crate::use_cases::testing::document(
                    r#"{"edit_policy":"advise","areas":[{"name":"billing","paths":["packages/billing/**"]}]}"#,
                ),
            )
            .await
            .unwrap();
        let result = check(&env, &env.bob, &["default:packages/billing/a.rs"], true).await;
        assert_eq!(verdicts(&result), [Verdict::Allowed]);
    }
}

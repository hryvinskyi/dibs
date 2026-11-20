//! Asking, for each path a member is about to change, whether they may.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use dibs_domain::area::QualifiedArea;
use dibs_domain::claim::Claim;
use dibs_domain::edit::{self, EditCheckInput as DomainInput, Verdict, edit_verdicts};
use dibs_domain::event::{EventKind, Reserved};
use dibs_domain::grant::Grant;
use dibs_domain::hot_rule::HotRuleRegistry;
use dibs_domain::ids::{ItemId, SessionId};
use dibs_domain::paths::RepoPath;
use dibs_domain::radar::{BranchSource, WorktreeRef};
use dibs_domain::ref_scheme::RefSchemeRegistry;
use dibs_domain::reservation::ReservationPlan;

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::named::{NamedVerdict, Names, item_ref};
use crate::ports::{Clock, EventNotifier, IdGen, Store};
use crate::use_cases::common::own_session;
use crate::use_cases::radar::{FromCheckout, Radar, fed_by_checkout, report_checkout};
use crate::use_cases::wide_asks::Announcer;
use crate::use_cases::worktrees::edit_in_worktree;
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
    /// The checkout the edits are made in, and the branch it is on when it is on one. A check
    /// that reserves stores that the session reported working there, and may record the branch
    /// on the one claim the checkout feeds.
    pub checkout: Option<FromCheckout>,
    /// The worktree of one of the caller's pools the edits are made in, when they are made in
    /// one. A check that reserves may teach its hold the item the edits are for, and records the
    /// checkout's branch on the claim on the item the hold names.
    pub worktree: Option<WorktreeRef>,
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
    /// The verdicts with the members they name by handle and the holders' items by reference.
    async fn named(
        check: edit::EditCheck,
        names: &Names,
        work: &mut Work,
    ) -> Result<Self, AppError> {
        let mut verdicts = Vec::with_capacity(check.verdicts.len());
        for verdict in check.verdicts {
            let reference = if let Verdict::HeldBy { item, .. } = &verdict.verdict {
                item_ref(work, *item).await?
            } else {
                None
            };
            let waiting_ref = match &verdict.verdict {
                Verdict::HeldBy {
                    reason:
                        edit::HeldReason::AskHolder {
                            waiting: Some(waiting),
                        },
                    ..
                } => item_ref(work, waiting.item).await?,
                _ => None,
            };
            let mut touched_refs = Vec::new();
            if let Verdict::AllowedWithNote { touched_by, .. } = &verdict.verdict {
                for holder in touched_by {
                    touched_refs.push(item_ref(work, holder.item).await?);
                }
            }
            verdicts.push(names.verdict(verdict, reference, touched_refs, waiting_ref)?);
        }
        Ok(Self {
            verdicts,
            plan: check.plan,
            touch: check.touch,
        })
    }
}

/// Answers a verdict per path. When the edits are being made, it records what the domain planned —
/// areas added to the caller's claims, paths recorded as shared — and refreshes the claims the edits
/// were attributed to, wherever those claims were taken. A `Reserved` event is appended per claim
/// that actually gained something, so a repeated check appends nothing. An area an edit adds to the
/// caller's claim that another member's live wide claim names makes the caller one of that wide
/// claim's holders, so its announcement is sent to them in the same transaction.
///
/// A check that reserves and names its checkout stores that its session reported working there
/// now. When the checkout is on a branch, that branch, learned from the checkout, is recorded on
/// one claim at most: the one live claim the session took or last renewed and works on, when the
/// edits were attributed to it, the session reported no other checkout and no other session of
/// the member reported this one, within the project's `stale_after`. A branch the claim learned
/// from a stronger source that still holds is kept. A `BranchRecorded` event marks a change of
/// branch, and the claim's footprint of the repository, which measured the branch before, goes
/// with what it made.
///
/// A check that reserves and names a worktree of the caller's pool it is made in teaches the
/// worktree's hold the item its edits are for: when the hold is held under the check's session, by
/// the session or one of its agents, has not gone silent and names no item, and the edits were
/// attributed to the claims of exactly one item, the hold takes that item; a hold that names one
/// keeps it, and learning appends no event. Whenever that hold names an item, the checkout's branch
/// is recorded on the caller's live claim on it, learned from that worktree of that pool, as a
/// worktree's signs of life record it; that recording comes before the checkout's own, which then
/// waits while the worktree holds.
pub struct EditCheck {
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGen>,
    notifier: Arc<dyn EventNotifier>,
    hot_rules: Arc<HotRuleRegistry>,
    ref_schemes: Arc<RefSchemeRegistry>,
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
            ids: deps.ids.clone(),
            notifier: deps.notifier.clone(),
            hot_rules: deps.hot_rules.clone(),
            ref_schemes: deps.ref_schemes.clone(),
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
        let mut work = Work::begin_for(&*self.store, actor).await?;
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
            return EditVerdicts::named(check, &names, &mut work).await;
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
        let stale_after = settings.stale_after();
        let on = input
            .checkout
            .as_ref()
            .and_then(|from| from.branch.as_ref())
            .filter(|on| settings.repo(&on.repo).is_some());
        let mut fed = None;
        if let Some(from) = &input.checkout {
            let reported =
                report_checkout(&mut work, actor.member, input.session, &from.token, now).await?;
            if on.is_some() {
                let touched = |claim: &Claim| check.touch.contains(&claim.item);
                fed = fed_by_checkout(&mut work, &reported, &claims, stale_after, touched)
                    .await?
                    .map(|claim| claim.item);
            }
        }
        let mut held_for = None;
        if let Some(worktree) = &input.worktree {
            let session = input.session;
            held_for = edit_in_worktree(
                &mut work,
                actor.member,
                session,
                worktree,
                &check.touch,
                now,
            )
            .await?
            .map(|item| (item, worktree.clone()));
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
        if let Some(on) = on {
            let radar = Radar {
                ids: &*self.ids,
                schemes: &self.ref_schemes,
                settings: &settings,
                now,
            };
            if let Some((item, worktree)) = held_for {
                radar
                    .record_worktree_branch(
                        &mut work,
                        actor.member,
                        item,
                        &on.repo,
                        &on.branch,
                        worktree,
                    )
                    .await?;
            }
            let fed = match fed {
                Some(item) => work.claim(project, item).await?,
                None => None,
            };
            if let Some(claim) = fed {
                let source = BranchSource::Checkout;
                radar
                    .record_branch(&mut work, &claim, &on.repo, &on.branch, source)
                    .await?;
            }
        }
        let held_more = gains.values().any(|gain| !gain.areas.is_empty());
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
        if held_more {
            Announcer {
                ids: &*self.ids,
                schemes: &self.ref_schemes,
                settings: &settings,
                now,
            }
            .announce_due(&mut work)
            .await?;
        }
        let verdicts = EditVerdicts::named(check, &names, &mut work).await?;
        work.commit(&*self.notifier).await?;
        Ok(verdicts)
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
                    branches: Default::default(),
                    wide: None,
                    item,
                    session: who.session,
                    areas: areas.iter().map(|a| (*a).to_owned()).collect(),
                    kind: None,
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
                    checkout: None,
                    worktree: None,
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
    async fn a_reserving_check_records_its_checkouts_branch_only_on_the_claim_its_edits_touch() {
        use dibs_domain::radar::{BranchSource, BranchSourceKind};

        use crate::use_cases::testing::{START, in_checkout};

        let env = Env::new().await;
        let item = env.item("gh:acme/widgets#1").await;
        claim(&env, &env.bob, item, &[]).await;
        let edit = |paths: &[&str], reserve: bool, branch: &str| EditCheckInput {
            session: env.bob.session,
            paths: paths.iter().map(|p| path(p)).collect(),
            reserve,
            checkout: Some(in_checkout('a', Some(branch))),
            worktree: None,
        };
        let check = EditCheck::new(&env.deps);
        let before = env.events().await.len();
        let asked = edit(&["default:packages/billing/a.rs"], false, "bob/1-asked");
        check.execute(&env.bob.actor, asked).await.unwrap();
        assert_eq!(env.events().await.len(), before);
        let mut tx = env.deps.store.begin().await.unwrap();
        let member = env.bob.actor.member;
        let stored = tx.session_checkouts(env.project, member, START).await;
        assert!(
            stored.unwrap().is_empty(),
            "a check that only asks stores nothing"
        );
        drop(tx);
        let untouched = edit(&["default:README.md"], true, "bob/1-readme");
        let result = check.execute(&env.bob.actor, untouched).await.unwrap();
        assert!(result.touch.is_empty());
        let touched = edit(&["default:packages/billing/a.rs"], true, "bob/1-x");
        check.execute(&env.bob.actor, touched).await.unwrap();
        let recorded: Vec<(String, BranchSourceKind)> = env
            .kinds_after(before)
            .await
            .into_iter()
            .filter_map(|kind| match kind {
                EventKind::BranchRecorded(recorded) => {
                    Some((recorded.branch.to_string(), recorded.source))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            recorded,
            [("bob/1-x".to_owned(), BranchSourceKind::Checkout)],
            "never from an edit the claim was not given"
        );
        let mut tx = env.deps.store.begin().await.unwrap();
        let stored = tx.claim(env.project, item).await.unwrap().unwrap();
        let repo = dibs_domain::repo::RepoId::implicit();
        assert_eq!(stored.branch_source(&repo), Some(&BranchSource::Checkout));
        let reported = tx.session_checkouts(env.project, member, START).await;
        assert_eq!(reported.unwrap().len(), 1);
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
                    branches: Default::default(),
                    wide: None,
                    item,
                    session: laptop,
                    areas: vec!["billing".to_owned()],
                    kind: None,
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
                reason: HeldReason::AskHolder { waiting: None },
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
        assert_eq!(
            second.verdicts[0].touched_refs,
            [Some(
                dibs_domain::item::ExternalRef::parse("gh:acme/widgets#1").unwrap()
            )],
            "each editor's item by its reference, in their order"
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
                    checkout: None,
                    worktree: None,
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
                    checkout: None,
                    worktree: None,
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

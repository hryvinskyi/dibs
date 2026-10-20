//! Sending, inside the caller's transaction, the announcements wide claims owe the holders of their
//! areas, and turning a holder's acknowledgement of one into their grants.
//!
//! An announcement is a thread about the wide claim's item, addressed to the one holder it asks,
//! whose single message is `high` and asks to be acknowledged. It is written in the name of the wide
//! claim's member, under the session label `dibs`, and its text says that dibs sent it for them:
//! the member did not type it, and the holder's answer is to them.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use time::OffsetDateTime;

use dibs_domain::area::QualifiedArea;
use dibs_domain::event::{EventKind, MessagePosted, ThreadStarted, WideGrantedByAck};
use dibs_domain::grant::Grant;
use dibs_domain::handle::Handle;
use dibs_domain::ids::{GrantId, MessageId, ThreadId};
use dibs_domain::message::{Body, Importance, Recipients, SessionLabel, Subject};
use dibs_domain::ref_scheme::RefSchemeRegistry;
use dibs_domain::settings::ProjectSettings;
use dibs_domain::wide::{AskDue, WideAsk, asks_due, grants_by_ack};

use crate::error::AppError;
use crate::named::{ItemName, NamedGrant, Names};
use crate::ports::{IdGen, StoreError};
use crate::records::{GrantRecord, MessageRecord, ThreadRecord};
use crate::use_cases::common::existing_item;
use crate::work::Work;

/// The session label announcements are written under.
const LABEL: &str = "dibs";

/// How many areas the text of an announcement lists by name before it counts the rest.
const LISTED_AREAS: usize = 20;

/// The most characters a subject holds.
const SUBJECT_MAX_CHARS: usize = 200;

/// An announcement sent, with the handle of the holder it asks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Announced {
    /// The announcement.
    pub ask: WideAsk,
    /// The thread that carries it.
    pub thread: ThreadId,
    /// The handle of `ask.to`.
    pub to: Handle,
}

/// What sending announcements needs besides the transaction.
pub(crate) struct Announcer<'a> {
    pub(crate) ids: &'a dyn IdGen,
    pub(crate) schemes: &'a RefSchemeRegistry,
    pub(crate) settings: &'a ProjectSettings,
    pub(crate) now: OffsetDateTime,
}

impl Announcer<'_> {
    /// Sends every announcement the project's claims, as they stand in the transaction now, owe:
    /// one per holder of an area a live wide claim names that nothing asked them about yet. Run it
    /// after any change that can make a member hold an area or widen a wide claim.
    pub(crate) async fn announce_due(&self, work: &mut Work) -> Result<Vec<Announced>, AppError> {
        let project = work.project_id();
        let claims = work.claims(project).await?;
        let grants: Vec<Grant> = work
            .grants(project)
            .await?
            .into_iter()
            .map(|record| record.grant)
            .collect();
        let open = work.open_wide_asks(project).await?;
        let due = asks_due(
            &claims,
            &grants,
            &open,
            self.now,
            self.settings.stale_after(),
        );
        if due.is_empty() {
            return Ok(Vec::new());
        }
        let names = Names::load(work).await?;
        let mut sent = Vec::with_capacity(due.len());
        for ask in due {
            let reason = claims
                .iter()
                .find(|claim| claim.item == ask.item)
                .and_then(|claim| claim.wide.as_ref())
                .map(|scope| scope.reason().as_str().to_owned())
                .unwrap_or_default();
            sent.push(self.send(work, &names, ask, &reason).await?);
        }
        Ok(sent)
    }

    async fn send(
        &self,
        work: &mut Work,
        names: &Names,
        due: AskDue,
        reason: &str,
    ) -> Result<Announced, AppError> {
        let project = work.project_id();
        let item = ItemName::of(&existing_item(work, due.item).await?, self.schemes);
        let from = names.handle(due.from)?;
        let to = names.handle(due.to)?;
        let thread = ThreadRecord {
            id: ThreadId(self.ids.next()),
            subject: subject(&from, &due.areas, &item)?,
            item: Some(due.item),
            author: due.from,
            recipients: Recipients::Members([due.to].into()),
            created_at: self.now,
        };
        let message_id = MessageId(self.ids.next());
        let message = MessageRecord {
            id: message_id,
            thread: thread.id,
            author: due.from,
            label: label()?,
            body: body(&from, &due.areas, &item, reason, message_id)?,
            importance: Importance::High,
            ack_required: true,
            created_at: self.now,
        };
        work.insert_thread(project, &thread).await?;
        work.insert_message(project, &message).await?;
        let ask = WideAsk {
            message: message_id,
            item: due.item,
            from: due.from,
            to: due.to,
            areas: due.areas,
        };
        work.insert_wide_ask(project, &ask).await?;
        work.record(
            self.now,
            EventKind::ThreadStarted(ThreadStarted {
                thread: thread.id,
                author: due.from,
                item: Some(due.item),
            }),
        )
        .await?;
        work.record(
            self.now,
            EventKind::MessagePosted(MessagePosted {
                thread: thread.id,
                message: message_id,
                author: due.from,
            }),
        )
        .await?;
        Ok(Announced {
            ask,
            thread: thread.id,
            to,
        })
    }
}

/// The grants a holder's acknowledgement of a wide claim's announcement makes, stored with one
/// event each, inside the caller's transaction. A message that carries no announcement grants
/// nothing.
pub(crate) async fn grant_by_ack(
    work: &mut Work,
    ids: &dyn IdGen,
    message: MessageId,
    settings: &ProjectSettings,
    now: OffsetDateTime,
) -> Result<Vec<NamedGrant>, AppError> {
    let project = work.project_id();
    let Some(ask) = work.wide_ask(project, message).await? else {
        return Ok(Vec::new());
    };
    let claims = work.claims(project).await?;
    let existing: Vec<Grant> = work
        .grants(project)
        .await?
        .into_iter()
        .map(|record| record.grant)
        .collect();
    let granted = grants_by_ack(&ask, &existing, &claims, now, settings);
    if granted.is_empty() {
        return Ok(Vec::new());
    }
    let names = Names::load(work).await?;
    let mut named = Vec::with_capacity(granted.len());
    for grant in granted {
        let record = GrantRecord {
            id: GrantId(ids.next()),
            grant,
            created_at: now,
        };
        work.insert_grant(project, &record).await?;
        work.record(
            now,
            EventKind::WideGrantedByAck(WideGrantedByAck {
                grant: record.id,
                item: record.grant.item,
                area: record.grant.area.clone(),
                by: record.grant.from,
                to: record.grant.to,
            }),
        )
        .await?;
        named.push(names.grant(record)?);
    }
    Ok(named)
}

fn label() -> Result<SessionLabel, AppError> {
    SessionLabel::parse(LABEL).map_err(|_| unwritable("the label of an announcement"))
}

/// The subject: who asks, for which areas, about which item — shortened to fit.
fn subject(
    from: &Handle,
    areas: &BTreeSet<QualifiedArea>,
    item: &ItemName,
) -> Result<Subject, AppError> {
    let text = format!(
        "Wide change by {from} on {}: may they edit {}?",
        item_label(item),
        area_list(areas)
    );
    let shortened: String = text.chars().take(SUBJECT_MAX_CHARS).collect();
    Subject::parse(shortened.trim_end()).map_err(|_| unwritable("the subject of an announcement"))
}

/// The message: that dibs sends it for the wide claim's member, why, which of the holder's areas
/// it is about, and what acknowledging it does.
fn body(
    from: &Handle,
    areas: &BTreeSet<QualifiedArea>,
    item: &ItemName,
    reason: &str,
    message: MessageId,
) -> Result<Body, AppError> {
    let listed = area_list(areas);
    let mut text = format!(
        "dibs sent this for {from}, who took a wide claim on work item {} \"{}\" for a change across several areas.",
        item_label(item),
        item.title
    );
    if !reason.is_empty() {
        let _ = write!(text, "\nWhy: {reason}");
    }
    let _ = write!(
        text,
        "\nYou hold {listed}. A wide claim holds none of the areas it names, so {from}'s edits there wait for your answer.\
         \nAcknowledge this message (dibs_ack, or dibs ack {message}) to grant {from} edits in {listed} for that work item; the grant ends when either claim ends.\
         \nIf the change should not happen there now, do not acknowledge it: reply in this thread instead."
    );
    Body::parse(&text).map_err(|_| unwritable("the text of an announcement"))
}

/// How an item is named in an announcement: its reference, or `#<id>`.
fn item_label(item: &ItemName) -> String {
    item.external_ref
        .as_ref()
        .map_or_else(|| format!("#{}", item.id), ToString::to_string)
}

/// The areas by name, the first [`LISTED_AREAS`] of them, with a count of the rest.
fn area_list(areas: &BTreeSet<QualifiedArea>) -> String {
    let named: Vec<String> = areas
        .iter()
        .take(LISTED_AREAS)
        .map(ToString::to_string)
        .collect();
    let mut text = named.join(", ");
    if areas.len() > LISTED_AREAS {
        let _ = write!(text, " and {} more", areas.len() - LISTED_AREAS);
    }
    text
}

/// A text this module composes broke the rules of its kind: something stored holds a value it
/// cannot carry.
fn unwritable(what: &str) -> AppError {
    AppError::Store(StoreError::Corrupt {
        what: format!("{what} could not be written from the stored records"),
    })
}

#[cfg(test)]
mod tests {
    use time::Duration;

    use dibs_domain::claim::ClaimRefused;
    use dibs_domain::edit::{HeldReason, Verdict};
    use dibs_domain::event::{GrantEnd, GrantRevoked, WideClaimed};
    use dibs_domain::ids::ItemId;
    use dibs_domain::member::Role;
    use dibs_domain::paths::RepoPath;

    use super::*;
    use crate::actor::Actor;
    use crate::named::Refusal;
    use crate::use_cases::testing::{Env, Person, new_member, session};
    use crate::use_cases::{
        AckMessage, Authenticate, ClaimItem, ClaimItemInput, ClaimMade, CreateMember,
        CreateMemberInput, EditCheck, EditCheckInput, Release, Status, TakeOver, TakeOverInput,
        Unacked, WideInput,
    };

    fn area(text: &str) -> QualifiedArea {
        QualifiedArea::parse(text).unwrap()
    }

    async fn carol(env: &Env) -> Person {
        let created = CreateMember::new(&env.deps)
            .execute(
                &env.alice.actor,
                CreateMemberInput {
                    member: new_member("carol"),
                    role: Role::Member,
                },
            )
            .await
            .unwrap();
        let actor = Authenticate::new(&env.deps)
            .execute(created.token.expose())
            .await
            .unwrap();
        let session = session(&env.deps, &actor, "tower", "tower/0c0ffee0").await;
        Person { actor, session }
    }

    async fn claim(env: &Env, who: &Person, item: ItemId, areas: &[&str]) -> ClaimMade {
        ClaimItem::new(&env.deps)
            .execute(
                &who.actor,
                ClaimItemInput {
                    item,
                    session: who.session,
                    areas: areas.iter().map(|a| (*a).to_owned()).collect(),
                    wide: None,
                },
            )
            .await
            .unwrap()
    }

    fn wide_input(item: ItemId, who: &Person, named: &[&str]) -> ClaimItemInput {
        ClaimItemInput {
            item,
            session: who.session,
            areas: Vec::new(),
            wide: Some(WideInput {
                areas: named.iter().map(|a| (*a).to_owned()).collect(),
                reason: dibs_domain::wide::WideReason::parse("Upgrade the HTTP client").unwrap(),
            }),
        }
    }

    async fn wide(env: &Env, who: &Person, item: ItemId, named: &[&str]) -> ClaimMade {
        ClaimItem::new(&env.deps)
            .execute(&who.actor, wide_input(item, who, named))
            .await
            .unwrap()
    }

    async fn unacked(env: &Env, who: &Actor) -> Vec<MessageRecord> {
        Unacked::new(&env.deps)
            .execute(who)
            .await
            .unwrap()
            .into_iter()
            .map(|unacked| unacked.message.record)
            .collect()
    }

    async fn verdict(env: &Env, who: &Person, path: &str) -> Verdict {
        EditCheck::new(&env.deps)
            .execute(
                &who.actor,
                EditCheckInput {
                    session: who.session,
                    paths: vec![RepoPath::parse(path).unwrap()],
                    reserve: true,
                },
            )
            .await
            .unwrap()
            .verdicts
            .remove(0)
            .verdict
            .verdict
    }

    #[tokio::test]
    async fn a_wide_claim_announces_once_to_each_holder_and_never_to_its_member() {
        let env = Env::new().await;
        let carol = carol(&env).await;
        let alices = env.item("gh:acme/widgets#1").await;
        let carols = env.item("gh:acme/widgets#2").await;
        let bobs = env.item("gh:acme/widgets#3").await;
        claim(&env, &env.alice, alices, &["billing"]).await;
        claim(&env, &carol, carols, &["search"]).await;
        let before = env.events().await.len();
        let made = wide(&env, &env.bob, bobs, &["billing", "search"]).await;
        let claim = made.outcome.claim();
        assert!(
            claim.areas.is_empty(),
            "a wide claim holds none of its areas"
        );
        let asked: Vec<(&str, BTreeSet<QualifiedArea>)> = made
            .announced
            .iter()
            .map(|a| (a.to.as_str(), a.ask.areas.clone()))
            .collect();
        assert_eq!(
            asked,
            [
                ("alice", [area("default:billing")].into()),
                ("carol", [area("default:search")].into()),
            ]
        );
        for who in [&env.alice.actor, &carol.actor] {
            let messages = unacked(&env, who).await;
            assert_eq!(messages.len(), 1, "one announcement each");
            assert_eq!(messages[0].importance, Importance::High);
            assert!(messages[0].ack_required);
            assert_eq!(messages[0].author, env.bob.actor.member);
            assert_eq!(messages[0].label.as_str(), "dibs");
            assert!(messages[0].body.as_str().contains("dibs sent this for bob"));
        }
        assert!(unacked(&env, &env.bob.actor).await.is_empty());
        let kinds = env.kinds_after(before).await;
        assert!(kinds.contains(&EventKind::WideClaimed(WideClaimed {
            item: bobs,
            member: env.bob.actor.member,
            areas: [area("default:billing"), area("default:search")].into(),
        })));
        let again = wide(&env, &env.bob, bobs, &["billing"]).await;
        assert!(again.announced.is_empty(), "a renewal asks nobody again");
        assert_eq!(unacked(&env, &env.alice.actor).await.len(), 1);
    }

    #[tokio::test]
    async fn the_wide_claimers_edit_waits_for_the_holders_ack_which_grants_it() {
        let env = Env::new().await;
        let alices = env.item("gh:acme/widgets#1").await;
        let bobs = env.item("gh:acme/widgets#2").await;
        claim(&env, &env.alice, alices, &["billing"]).await;
        let made = wide(&env, &env.bob, bobs, &["billing", "search"]).await;
        let path = "default:packages/billing/src/Invoice.php";
        assert!(matches!(
            verdict(&env, &env.bob, path).await,
            Verdict::HeldBy {
                reason: HeldReason::AwaitingAck,
                ..
            }
        ));
        assert_eq!(
            verdict(&env, &env.bob, "default:packages/search/src/Index.php").await,
            Verdict::Allowed,
            "an area nobody holds is free to the wide claimer"
        );
        let message = made.announced[0].ask.message;
        let before = env.events().await.len();
        let acked = AckMessage::new(&env.deps)
            .execute(&env.alice.actor, message)
            .await
            .unwrap();
        assert_eq!(acked.granted.len(), 1);
        let grant = &acked.granted[0];
        assert_eq!(
            (
                grant.record.grant.item,
                grant.from.as_str(),
                grant.to.as_str()
            ),
            (bobs, "alice", "bob")
        );
        let kinds = env.kinds_after(before).await;
        assert_eq!(
            kinds.last(),
            Some(&EventKind::WideGrantedByAck(WideGrantedByAck {
                grant: grant.record.id,
                item: bobs,
                area: area("default:billing"),
                by: env.alice.actor.member,
                to: env.bob.actor.member,
            }))
        );
        assert_eq!(
            verdict(&env, &env.bob, path).await,
            Verdict::Granted {
                by: env.alice.actor.member,
                area: area("default:billing"),
            }
        );
        let repeat = AckMessage::new(&env.deps)
            .execute(&env.alice.actor, message)
            .await
            .unwrap();
        assert!(repeat.granted.is_empty(), "a repeat grants nothing again");
        let status = Status::new(&env.deps)
            .execute(&env.alice.actor)
            .await
            .unwrap();
        assert_eq!(status.wide.len(), 1);
        let rows: Vec<(String, Option<&str>, bool)> = status.wide[0]
            .areas
            .iter()
            .map(|row| {
                (
                    row.area.to_string(),
                    row.holder.as_ref().map(|(h, _)| h.as_str()),
                    row.granted,
                )
            })
            .collect();
        assert_eq!(
            rows,
            [
                ("default:billing".to_owned(), Some("alice"), true),
                ("default:search".to_owned(), None, false),
            ]
        );
        assert!(
            status.holds.iter().all(|hold| hold.item != bobs),
            "the wide claim holds nothing"
        );
    }

    #[tokio::test]
    async fn the_grant_by_ack_ends_with_the_holders_claim() {
        let env = Env::new().await;
        let alices = env.item("gh:acme/widgets#1").await;
        let bobs = env.item("gh:acme/widgets#2").await;
        claim(&env, &env.alice, alices, &["billing"]).await;
        let made = wide(&env, &env.bob, bobs, &["billing"]).await;
        let acked = AckMessage::new(&env.deps)
            .execute(&env.alice.actor, made.announced[0].ask.message)
            .await
            .unwrap();
        let before = env.events().await.len();
        Release::new(&env.deps)
            .execute(&env.alice.actor, alices)
            .await
            .unwrap();
        assert!(
            env.kinds_after(before)
                .await
                .contains(&EventKind::GrantRevoked(GrantRevoked {
                    grant: acked.granted[0].record.id,
                    item: bobs,
                    area: area("default:billing"),
                    from: env.alice.actor.member,
                    to: env.bob.actor.member,
                    cause: GrantEnd::GrantorLeft,
                }))
        );
    }

    #[tokio::test]
    async fn a_member_who_claims_inside_a_live_wide_claim_is_told_and_asked_at_once() {
        let env = Env::new().await;
        let bobs = env.item("gh:acme/widgets#1").await;
        let alices = env.item("gh:acme/widgets#2").await;
        wide(&env, &env.bob, bobs, &["billing"]).await;
        let before = env.events().await.len();
        let made = claim(&env, &env.alice, alices, &["billing"]).await;
        assert!(made.outcome.claim().holds(&area("default:billing")));
        let asked = unacked(&env, &env.alice.actor).await;
        assert_eq!(asked.len(), 1, "the new holder is sent the announcement");
        assert_eq!(made.inside_wide.len(), 1);
        let note = &made.inside_wide[0];
        assert_eq!(note.area, area("default:billing"));
        assert_eq!(note.holder.as_str(), "bob");
        assert_eq!(note.item.id, bobs);
        assert_eq!(note.message, Some(asked[0].id));
        assert!(
            made.announced.is_empty(),
            "nothing was announced in alice's name"
        );
        let kinds = env.kinds_after(before).await;
        assert!(
            kinds
                .iter()
                .any(|kind| matches!(kind, EventKind::MessagePosted(posted) if posted.message == asked[0].id)),
            "in the same transaction as the claim"
        );
        let renewed = claim(&env, &env.alice, alices, &["billing"]).await;
        assert_eq!(renewed.inside_wide[0].message, Some(asked[0].id));
        assert_eq!(unacked(&env, &env.alice.actor).await.len(), 1, "asked once");
    }

    #[tokio::test]
    async fn a_holder_who_comes_to_hold_an_area_by_editing_is_asked() {
        let env = Env::new().await;
        let bobs = env.item("gh:acme/widgets#1").await;
        let alices = env.item("gh:acme/widgets#2").await;
        wide(&env, &env.bob, bobs, &["billing"]).await;
        claim(&env, &env.alice, alices, &[]).await;
        assert!(unacked(&env, &env.alice.actor).await.is_empty());
        assert_eq!(
            verdict(&env, &env.alice, "default:packages/billing/a.php").await,
            Verdict::Allowed
        );
        assert_eq!(unacked(&env, &env.alice.actor).await.len(), 1);
    }

    #[tokio::test]
    async fn two_wide_claims_over_one_area_refuse_the_second_naming_the_first() {
        let env = Env::new().await;
        let bobs = env.item("gh:acme/widgets#1").await;
        let alices = env.item("gh:acme/widgets#2").await;
        wide(&env, &env.bob, bobs, &["billing"]).await;
        let error = ClaimItem::new(&env.deps)
            .execute(
                &env.alice.actor,
                wide_input(alices, &env.alice, &["search", "billing"]),
            )
            .await
            .unwrap_err();
        assert_eq!(error.code(), "wide_overlap");
        assert!(matches!(
            &error,
            AppError::Claim(Refusal {
                reason: ClaimRefused::WideOverlap { item, .. },
                member: Some(handle),
                ..
            }) if *item == bobs && handle.as_str() == "bob"
        ));
        let empty = ClaimItem::new(&env.deps)
            .execute(&env.alice.actor, wide_input(alices, &env.alice, &[]))
            .await
            .unwrap_err();
        assert_eq!(empty.code(), "invalid_wide");
    }

    #[tokio::test]
    async fn a_takeover_carries_the_wide_scope_and_asks_again_in_the_new_name() {
        let env = Env::new().await;
        let carol = carol(&env).await;
        let bobs = env.item("gh:acme/widgets#1").await;
        let carols = env.item("gh:acme/widgets#2").await;
        wide(&env, &env.bob, bobs, &["billing"]).await;
        env.advance(Duration::hours(25));
        claim(&env, &carol, carols, &["billing"]).await;
        assert!(
            unacked(&env, &carol.actor).await.is_empty(),
            "a stale wide claim asks nobody"
        );
        let taken = TakeOver::new(&env.deps)
            .execute(
                &env.alice.actor,
                TakeOverInput {
                    item: bobs,
                    session: env.alice.session,
                    areas: Vec::new(),
                    reason: Body::parse("Bob is away; I finish the upgrade.").unwrap(),
                },
            )
            .await
            .unwrap();
        assert!(taken.takeover.claim.names_wide(&area("default:billing")));
        assert_eq!(taken.announced.len(), 1);
        let asked = unacked(&env, &carol.actor).await;
        assert_eq!(asked.len(), 1);
        assert_eq!(asked[0].author, env.alice.actor.member);
    }
}

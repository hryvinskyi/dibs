//! Who holds what right now.

use std::sync::Arc;

use time::OffsetDateTime;

use dibs_domain::area::QualifiedArea;
use dibs_domain::grant::Grant;
use dibs_domain::handle::Handle;
use dibs_domain::hot_rule::HotRuleRegistry;
use dibs_domain::ids::{ItemId, MemberId};
use dibs_domain::integration::{IntegrationSign, review_in_use, silent_since};
use dibs_domain::ref_scheme::RefSchemeRegistry;
use dibs_domain::wide::standing;

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::named::{ItemName, NamedClaim, NamedItem, Names};
use crate::ports::{Clock, EventNotifier, IdGen, Store, StoreError};
use crate::use_cases::escalate::{OpenEscalation, open_escalations};
use crate::use_cases::radar::{NamedOverlap, live_overlaps};
use crate::use_cases::ready::{ReadyScope, ReadyView};
use crate::use_cases::slots::{Scene, SlotLine, scene_lines};
use crate::work::Work;

/// A claim with its item, each member with their handle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldItem {
    /// The claim.
    pub claim: NamedClaim,
    /// The item.
    pub item: NamedItem,
    /// The tracker's page for the item, when its reference has one.
    pub link: Option<String>,
}

/// An area and the live claim holding it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AreaHold {
    /// The area.
    pub area: QualifiedArea,
    /// Its holder.
    pub member: MemberId,
    /// The holder's handle.
    pub holder: Handle,
    /// The item of the holder's claim.
    pub item: ItemId,
}

/// Where one area of a live wide claim stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WideAreaStanding {
    /// The area.
    pub area: QualifiedArea,
    /// The other member whose live claim holds it, with their handle and item; `None` when nobody
    /// else holds it and the wide claim's member may edit there.
    pub holder: Option<(Handle, ItemId)>,
    /// Whether that holder granted the edits there, by acknowledging the announcement or by a
    /// grant.
    pub granted: bool,
}

/// A live wide claim and where each area it names stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WideClaimStatus {
    /// The claim and its item.
    pub held: HeldItem,
    /// Its areas, in area order.
    pub areas: Vec<WideAreaStanding>,
}

/// The project's claims and holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusView {
    /// Live claims, ordered by item.
    pub live: Vec<HeldItem>,
    /// Stale claims, ordered by item: they hold nothing and wait for their holder or a takeover.
    pub stale: Vec<HeldItem>,
    /// The areas live claims hold, ordered by area. The areas a wide claim names are not among
    /// them.
    pub holds: Vec<AreaHold>,
    /// The live wide claims, ordered by item.
    pub wide: Vec<WideClaimStatus>,
    /// The work the caller can pick up now, and the open items that are not ready.
    pub ready: ReadyView,
    /// Every declared slot with its group's hold, queue and offer.
    pub slots: Vec<SlotLine>,
    /// The escalations the caller may read that no arbiter has acknowledged yet, newest first.
    pub escalations: Vec<OpenEscalation>,
    /// Since when the project's integrations have not called, once that is long enough to count
    /// as silent, in a project whose reviews rely on them — a claim is in review or a repository's
    /// pull requests get a status: merges then complete no claims and statuses go stale. `None`
    /// while one calls, when the project has none, or when nothing relies on them.
    pub integration_silent_since: Option<OffsetDateTime>,
    /// The overlaps the radar found between live claims, and the moved bases under them, in key
    /// order.
    pub overlaps: Vec<NamedOverlap>,
}

/// Shows the project's live and stale claims, the areas held, the live wide claims with where each
/// of their areas stands, the caller's ready list, where the slots stand — after applying, and
/// recording, what time changed about them — the escalations still waiting for an arbiter,
/// whether the project's integrations went silent, and the overlaps between live claims.
pub struct Status {
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGen>,
    notifier: Arc<dyn EventNotifier>,
    hot_rules: Arc<HotRuleRegistry>,
    ref_schemes: Arc<RefSchemeRegistry>,
}

impl Status {
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

    /// The project's status now.
    ///
    /// # Errors
    /// [`AppError::Forbidden`] for an integration; [`AppError::Store`] when the store fails.
    pub async fn execute(&self, actor: &Actor) -> Result<StatusView, AppError> {
        actor.require_person()?;
        let now = self.clock.now();
        let mut work = Work::begin_for(&*self.store, actor).await?;
        let mut scene = Scene::load(&mut work, &self.hot_rules, now).await?;
        scene.save(&mut work, &*self.ids, now).await?;
        let settings = scene.settings.clone();
        let stale_after = settings.stale_after();
        let names = Names::load(&mut work).await?;
        let ready = ReadyScope {
            me: actor.member,
            now,
            settings: &settings,
            schemes: &self.ref_schemes,
        }
        .view(&mut work, &names)
        .await?;
        let mut view = StatusView {
            live: Vec::new(),
            stale: Vec::new(),
            holds: Vec::new(),
            wide: Vec::new(),
            ready,
            slots: scene_lines(&mut work, &scene).await?,
            escalations: open_escalations(&mut work, actor, &names, &self.ref_schemes).await?,
            integration_silent_since: None,
            overlaps: Vec::new(),
        };
        let claims = work.claims(actor.project).await?;
        view.overlaps = live_overlaps(
            &mut work,
            &claims,
            &names,
            &settings,
            &self.ref_schemes,
            now,
        )
        .await?;
        if review_in_use(&claims, &settings) {
            view.integration_silent_since = integration_silence(&mut work, now).await?;
        }
        let grants: Vec<Grant> = work
            .grants(actor.project)
            .await?
            .into_iter()
            .map(|record| record.grant)
            .collect();
        for claim in claims.iter().cloned() {
            let item =
                work.item(actor.project, claim.item)
                    .await?
                    .ok_or_else(|| StoreError::Corrupt {
                        what: format!("claim on item {} that does not exist", claim.item),
                    })?;
            let link = ItemName::of(&item, &self.ref_schemes).link;
            let held = HeldItem {
                claim: names.claim(claim)?,
                item: names.item(item)?,
                link,
            };
            let claim = &held.claim;
            if claim.claim.is_live(now, stale_after) {
                view.holds
                    .extend(claim.claim.areas.iter().map(|area| AreaHold {
                        area: area.clone(),
                        member: claim.claim.member,
                        holder: claim.holder.clone(),
                        item: claim.claim.item,
                    }));
                if claim.claim.wide.is_some() {
                    let areas = standing(&claim.claim, &claims, &grants, now, stale_after)
                        .into_iter()
                        .map(|row| {
                            Ok(WideAreaStanding {
                                holder: row
                                    .holder
                                    .map(|holder| {
                                        names.handle(holder.member).map(|h| (h, holder.item))
                                    })
                                    .transpose()?,
                                area: row.area,
                                granted: row.granted,
                            })
                        })
                        .collect::<Result<_, AppError>>()?;
                    view.wide.push(WideClaimStatus {
                        held: held.clone(),
                        areas,
                    });
                }
                view.live.push(held);
            } else {
                view.stale.push(held);
            }
        }
        view.holds
            .sort_by(|a, b| (&a.area, a.item).cmp(&(&b.area, b.item)));
        work.commit(&*self.notifier).await?;
        Ok(view)
    }
}

/// Since when the project's present integrations have not called, when that counts as silent.
async fn integration_silence(
    work: &mut Work,
    now: OffsetDateTime,
) -> Result<Option<OffsetDateTime>, AppError> {
    let project = work.project_id();
    let calls = work.last_calls(project).await?;
    let signs: Vec<IntegrationSign> = work
        .members(project)
        .await?
        .into_iter()
        .filter(|member| member.is_present() && !member.role.is_person())
        .map(|member| IntegrationSign {
            joined: member.created_at,
            last_call: calls
                .iter()
                .find(|(id, _)| *id == member.id)
                .map(|(_, at)| *at),
        })
        .collect();
    Ok(silent_since(&signs, now))
}

#[cfg(test)]
mod tests {
    use time::Duration;

    use super::*;
    use crate::use_cases::testing::{Env, Person};
    use crate::use_cases::{ClaimItem, ClaimItemInput};

    async fn claim(env: &Env, who: &Person, item: ItemId, areas: &[&str]) {
        ClaimItem::new(&env.deps)
            .execute(
                &who.actor,
                ClaimItemInput {
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

    #[tokio::test]
    async fn live_and_stale_claims_and_holds_are_told_apart() {
        let env = Env::new().await;
        let old = env.item("gh:acme/widgets#1").await;
        let fresh = env.item("gh:acme/widgets#2").await;
        claim(&env, &env.alice, old, &["search"]).await;
        env.advance(Duration::hours(25));
        claim(&env, &env.bob, fresh, &["billing", "search"]).await;
        let view = Status::new(&env.deps)
            .execute(&env.alice.actor)
            .await
            .unwrap();
        let ids = |held: &[HeldItem]| held.iter().map(|h| h.item.item.id).collect::<Vec<_>>();
        assert_eq!(ids(&view.live), [fresh]);
        assert_eq!(ids(&view.stale), [old]);
        let holds: Vec<(String, ItemId)> = view
            .holds
            .iter()
            .map(|h| (h.area.to_string(), h.item))
            .collect();
        assert_eq!(
            holds,
            [
                ("default:billing".to_owned(), fresh),
                ("default:search".to_owned(), fresh)
            ]
        );
        assert!(view.holds.iter().all(|h| h.member == env.bob.actor.member));
        assert!(view.holds.iter().all(|h| h.holder.as_str() == "bob"));
        assert_eq!(view.live[0].claim.holder.as_str(), "bob");
        assert_eq!(
            view.stale[0].link.as_deref(),
            Some("https://github.com/acme/widgets/issues/1"),
            "a held item links to its tracker's page"
        );
    }

    #[tokio::test]
    async fn the_status_says_since_when_the_integrations_are_silent_while_reviews_rely_on_them() {
        let env = Env::with_settings(r#"{"pr_status": {"enabled": true}}"#).await;
        let quiet = Env::new().await;
        quiet.advance(Duration::minutes(16));
        assert_eq!(
            Status::new(&quiet.deps)
                .execute(&quiet.alice.actor)
                .await
                .unwrap()
                .integration_silent_since,
            None,
            "a project whose reviews do not rely on an integration hears no alarm"
        );
        let status = || async {
            Status::new(&env.deps)
                .execute(&env.alice.actor)
                .await
                .unwrap()
                .integration_silent_since
        };
        assert_eq!(status().await, None, "it signed in a moment ago");
        env.advance(Duration::minutes(16));
        let silent = status().await;
        assert_eq!(silent, Some(crate::use_cases::testing::START));
        let mut tx = env.deps.store.begin().await.unwrap();
        tx.record_call(env.project, env.importer.member, env.now())
            .await
            .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(status().await, None, "a call ends the silence");
    }
}

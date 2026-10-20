//! Narrowing a claim: its holder gives areas of it up.

use std::collections::BTreeSet;
use std::sync::Arc;

use dibs_domain::area::QualifiedArea;
use dibs_domain::claim::decide_narrow;
use dibs_domain::event::{ClaimNarrowed, EventKind};
use dibs_domain::handle::Handle;
use dibs_domain::handover::{AreaMove, WithdrawReason, Withdrawal};
use dibs_domain::hot_rule::HotRuleRegistry;
use dibs_domain::ids::ItemId;
use dibs_domain::ref_scheme::RefSchemeRegistry;

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::{AppError, Missing};
use crate::named::{ItemName, NamedClaim, Names, refuse};
use crate::ports::{Clock, EventNotifier, IdGen, Store};
use crate::use_cases::common::{existing_item, resolve_areas};
use crate::use_cases::grant_endings::end_orphaned_grants;
use crate::use_cases::handover::Handovers;
use crate::work::Work;

/// The narrowing to make.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NarrowClaimInput {
    /// The item, by local id.
    pub item: ItemId,
    /// The areas to give up, as a person names them: `repo:area`, or a bare name exactly one
    /// repository declares.
    pub areas: Vec<String>,
}

/// An area a narrowing let go that went to the claim waiting for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandedOver {
    /// The area.
    pub area: QualifiedArea,
    /// The item of the claim that holds it now.
    pub to: ItemName,
    /// The handle of that claim's holder.
    pub holder: Handle,
}

/// What a narrowing did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NarrowedClaim {
    /// The claim as it stands now, with its holder's handle.
    pub claim: NamedClaim,
    /// The item.
    pub item: ItemName,
    /// The areas it gave up: those asked for that it held or waited for.
    pub areas: BTreeSet<QualifiedArea>,
    /// The areas given up that another claim waited for, each with where it went.
    pub handed_over: Vec<HandedOver>,
}

/// Gives areas of the caller's claim up, live or stale: the claim neither holds them nor waits for
/// them any more, and the grants its holder gave in them end. An area it waited for ends that wait
/// (`wait_withdrawn`), and the debt holder asked for it is told that the request no longer stands.
/// In the same transaction an area another claim waited for to be handed over moves to that claim,
/// once no other member holds it, and its holder is told. An area
/// the claim neither holds nor waits for changes nothing, so a narrowing sent again does nothing
/// more.
pub struct NarrowClaim {
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGen>,
    notifier: Arc<dyn EventNotifier>,
    hot_rules: Arc<HotRuleRegistry>,
    ref_schemes: Arc<RefSchemeRegistry>,
}

impl NarrowClaim {
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

    /// Narrows the claim.
    ///
    /// # Errors
    /// [`AppError::Forbidden`] for an integration; [`AppError::NotFound`] for an unknown item;
    /// [`AppError::Area`] for an area that does not resolve; [`AppError::HolderOnly`] when nobody or
    /// another member holds the item; [`AppError::Store`] when the store fails.
    pub async fn execute(
        &self,
        actor: &Actor,
        input: NarrowClaimInput,
    ) -> Result<NarrowedClaim, AppError> {
        actor.require_person()?;
        let now = self.clock.now();
        let project = actor.project;
        let mut work = Work::begin_for(&*self.store, actor).await?;
        let item = existing_item(&mut work, input.item).await?;
        let settings = work.project_settings(&self.hot_rules).await?;
        let areas = resolve_areas(&settings, &input.areas)?;
        let current = work.claim(project, item.id).await?;
        let narrowed = match decide_narrow(current.as_ref(), actor.member, &areas, now) {
            Ok(narrowed) => narrowed,
            Err(reason) => return Err(refuse(&mut work, reason).await),
        };
        let others = work.claims(project).await?;
        work.save_claim(project, &narrowed.claim).await?;
        let handovers = Handovers {
            ids: &*self.ids,
            schemes: &self.ref_schemes,
            settings: &settings,
            now,
        };
        if !narrowed.withdrawn.is_empty() {
            let names = Names::load(&mut work).await?;
            for (area, pending) in &narrowed.withdrawn {
                let withdrawal = Withdrawal {
                    moved: AreaMove {
                        area: area.clone(),
                        from: pending.from,
                        to: item.id,
                    },
                    reason: WithdrawReason::Narrowed,
                    request: pending.request,
                };
                handovers
                    .withdraw(&mut work, &names, &others, &withdrawal)
                    .await?;
            }
        }
        if !narrowed.areas.is_empty() {
            work.record(
                now,
                EventKind::ClaimNarrowed(ClaimNarrowed {
                    item: item.id,
                    member: actor.member,
                    areas: narrowed.areas.clone(),
                }),
            )
            .await?;
            end_orphaned_grants(&mut work, now).await?;
        }
        let settled = handovers.settle(&mut work).await?;
        let names = Names::load(&mut work).await?;
        let mut handed_over = Vec::new();
        for moved in settled
            .handed_over
            .into_iter()
            .filter(|moved| moved.from == item.id)
        {
            let Some(receiving) = work.claim(project, moved.to).await? else {
                continue;
            };
            handed_over.push(HandedOver {
                to: ItemName::of(
                    &existing_item(&mut work, moved.to).await?,
                    &self.ref_schemes,
                ),
                holder: names.handle(receiving.member)?,
                area: moved.area,
            });
        }
        let stored = work
            .claim(project, item.id)
            .await?
            .ok_or(AppError::NotFound(Missing::Item(item.id)))?;
        let narrowed = NarrowedClaim {
            claim: names.claim(stored)?,
            item: ItemName::of(&item, &self.ref_schemes),
            areas: narrowed.areas,
            handed_over,
        };
        work.commit(&*self.notifier).await?;
        Ok(narrowed)
    }
}

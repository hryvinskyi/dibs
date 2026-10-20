//! End-to-end scenarios through the crate's public API only: a settings document parsed, members
//! claiming, edit checks answered and their plans applied, grants given and ended, claims released
//! and taken over, and the events those changes publish.

use std::collections::BTreeSet;

use dibs_domain::area::QualifiedArea;
use dibs_domain::claim::{
    Claim, ClaimOutcome, ClaimRefused, ClaimRequest, decide_claim, decide_release, decide_takeover,
};
use dibs_domain::edit::{EditCheck, EditCheckInput, HeldReason, Verdict, edit_verdicts};
use dibs_domain::event::{Claimed, EventKind, Reserved};
use dibs_domain::grant::{Grant, GrantOutcome, decide_grant};
use dibs_domain::hot_rule::HotRuleRegistry;
use dibs_domain::ids::{ItemId, MemberId, SessionId};
use dibs_domain::item::{CreateRefused, ExternalRef, ItemRepository, decide_create_item};
use dibs_domain::member::Role;
use dibs_domain::paths::{RelPath, RepoPath};
use dibs_domain::ref_scheme::RefSchemeRegistry;
use dibs_domain::repo::RepoId;
use dibs_domain::reservation::{ReservationPlan, SharedPath};
use dibs_domain::session::HostName;
use dibs_domain::settings::{ProjectSettings, SettingsDocument};
use time::{Duration, OffsetDateTime};

/// Fixtures of the scenarios: the project as a store would hold it between decisions.
#[cfg(test)]
mod support {
    use super::*;

    pub(crate) const ALICE: MemberId = MemberId(1);
    pub(crate) const BOB: MemberId = MemberId(2);

    pub(crate) const SETTINGS: &str = r#"{
        "stale_after": "24h",
        "edit_policy": "require-claim",
        "repos": [
            {
                "id": "api",
                "remote": "https://github.com/acme/api.git",
                "areas": [
                    {"name": "billing", "paths": ["packages/billing/**"]},
                    {"name": "search", "paths": ["packages/search/**"]}
                ],
                "hot": [
                    {"paths": ["**/*.lock"], "kind": "regenerate", "note": "regenerate, never merge by hand"},
                    {"paths": ["config/**"], "kind": "warn-shared"}
                ]
            },
            {
                "id": "web",
                "remote": "git@github.com:acme/web.git",
                "areas": [
                    {"name": "billing", "paths": ["src/billing/**"]},
                    {"name": "checkout", "paths": ["src/checkout/**"]}
                ]
            }
        ]
    }"#;

    /// The project as a store would hold it between decisions.
    pub(crate) struct World {
        pub(crate) settings: ProjectSettings,
        pub(crate) claims: Vec<Claim>,
        pub(crate) grants: Vec<Grant>,
        pub(crate) shared: Vec<SharedPath>,
        pub(crate) now: OffsetDateTime,
    }

    impl World {
        pub(crate) fn new() -> Self {
            let document: SettingsDocument = serde_json::from_str(SETTINGS).unwrap();
            let settings = ProjectSettings::parse(&document, &HotRuleRegistry::standard()).unwrap();
            Self {
                settings,
                claims: Vec::new(),
                grants: Vec::new(),
                shared: Vec::new(),
                now: OffsetDateTime::UNIX_EPOCH,
            }
        }

        pub(crate) fn area(&self, text: &str) -> QualifiedArea {
            self.settings.resolve_area(text).unwrap()
        }

        pub(crate) fn request(&self, item: u64, by: MemberId, areas: &[&str]) -> ClaimRequest {
            ClaimRequest {
                item: ItemId(item),
                by,
                session: SessionId(by.get() * 10),
                host: HostName::parse("laptop").unwrap(),
                areas: areas.iter().map(|a| self.area(a)).collect(),
                wide: None,
            }
        }

        pub(crate) fn claim(
            &mut self,
            item: u64,
            by: MemberId,
            areas: &[&str],
        ) -> Result<ClaimOutcome, ClaimRefused> {
            let request = self.request(item, by, areas);
            let current = self.claims.iter().find(|c| c.item == request.item);
            let outcome = decide_claim(current, &self.claims, &request, self.now, &self.settings)?;
            self.claims.retain(|c| c.item != request.item);
            self.claims.push(outcome.claim().clone());
            Ok(outcome)
        }

        pub(crate) fn check(&self, me: MemberId, paths: &[&str]) -> EditCheck {
            let paths: Vec<RepoPath> = paths.iter().map(|p| RepoPath::parse(p).unwrap()).collect();
            let input = EditCheckInput {
                me,
                claims: &self.claims,
                grants: &self.grants,
                shared: &self.shared,
                settings: &self.settings,
                now: self.now,
            };
            edit_verdicts(&paths, &input).unwrap()
        }

        pub(crate) fn apply(&mut self, check: &EditCheck) {
            let ReservationPlan {
                add_areas,
                share_paths,
            } = &check.plan;
            for add in add_areas {
                if let Some(claim) = self.claims.iter_mut().find(|c| c.item == add.claim) {
                    claim.areas.insert(add.area.clone());
                }
            }
            self.shared.extend(share_paths.iter().cloned());
            let now = self.now;
            for claim in self
                .claims
                .iter_mut()
                .filter(|c| check.touch.contains(&c.item))
            {
                claim.last_seen = now;
            }
        }

        /// Removes grants no longer in force, as a store does after a claim ends.
        pub(crate) fn drop_ended_grants(&mut self) {
            let (claims, now, stale_after) = (&self.claims, self.now, self.settings.stale_after());
            self.grants.retain(|g| g.in_force(claims, now, stale_after));
        }
    }

    pub(crate) fn verdict(check: &EditCheck) -> &Verdict {
        &check.verdicts[0].verdict
    }
}

use support::{ALICE, BOB, World, verdict};

#[test]
fn two_members_claim_edit_grant_and_release() {
    let mut world = World::new();

    world.claim(1, ALICE, &["api:billing"]).unwrap();
    world.claim(2, BOB, &["search"]).unwrap();

    let refused = world.claim(3, BOB, &["api:billing"]).unwrap_err();
    assert_eq!(
        refused,
        ClaimRefused::AreaHeld {
            area: world.area("api:billing"),
            member: ALICE,
            item: ItemId(1),
        }
    );

    let bob_in_billing = world.check(BOB, &["api:packages/billing/src/Invoice.php"]);
    assert_eq!(
        verdict(&bob_in_billing),
        &Verdict::HeldBy {
            member: ALICE,
            item: ItemId(1),
            area: world.area("api:billing"),
            reason: HeldReason::AskHolder,
        }
    );

    let grant = Grant {
        item: ItemId(2),
        area: world.area("api:billing"),
        from: ALICE,
        to: BOB,
    };
    let outcome = decide_grant(
        &grant,
        &world.grants,
        &world.claims,
        world.now,
        &world.settings,
    )
    .unwrap();
    assert_eq!(outcome, GrantOutcome::New(grant.clone()));
    world.grants.push(grant);

    let granted = world.check(BOB, &["api:packages/billing/src/Invoice.php"]);
    assert_eq!(
        verdict(&granted),
        &Verdict::Granted {
            by: ALICE,
            area: world.area("api:billing"),
        }
    );
    assert!(granted.plan.is_empty(), "a granted edit adds no area");

    let alice_claim = world.claims.iter().find(|c| c.item == ItemId(1)).cloned();
    let released = decide_release(alice_claim.as_ref(), ALICE).unwrap();
    world.claims.retain(|c| c.item != released.claim.item);
    world.drop_ended_grants();
    assert!(
        world.grants.is_empty(),
        "the grant ends with the grantor's claim"
    );

    let after = world.check(BOB, &["api:packages/billing/src/Invoice.php"]);
    assert_eq!(verdict(&after), &Verdict::Allowed);
    assert_eq!(
        after.plan.add_areas.len(),
        1,
        "the free area joins Bob's claim"
    );
    world.apply(&after);
    assert!(
        world
            .check(BOB, &["api:packages/billing/src/Invoice.php"])
            .plan
            .is_empty()
    );
}

#[test]
fn a_member_without_a_claim_is_told_to_claim_and_hot_files_speak_for_themselves() {
    let world = World::new();
    let check = world.check(
        ALICE,
        &[
            "api:packages/billing/src/Invoice.php",
            "api:composer.lock",
            "api:README.md",
        ],
    );
    let verdicts: Vec<&Verdict> = check.verdicts.iter().map(|v| &v.verdict).collect();
    assert_eq!(verdicts[0], &Verdict::NeedsClaim);
    assert!(
        matches!(verdicts[1], Verdict::AllowedWithNote { rule, .. } if rule.as_str() == "regenerate")
    );
    assert_eq!(verdicts[2], &Verdict::Allowed);
    assert!(check.plan.is_empty());
}

#[test]
fn one_claim_spans_two_repositories_and_names_repeat_across_them() {
    let mut world = World::new();
    world
        .claim(1, ALICE, &["api:billing", "web:checkout"])
        .unwrap();
    world.claim(2, BOB, &["web:billing"]).unwrap();

    let check = world.check(
        ALICE,
        &[
            "api:packages/billing/a.php",
            "web:src/checkout/Pay.ts",
            "web:src/billing/Cart.ts",
        ],
    );
    assert_eq!(check.verdicts[0].verdict, Verdict::Allowed);
    assert_eq!(check.verdicts[1].verdict, Verdict::Allowed);
    assert!(matches!(
        &check.verdicts[2].verdict,
        Verdict::HeldBy { member, area, .. } if *member == BOB && area.to_string() == "web:billing"
    ));
    assert_eq!(check.touch, BTreeSet::from([ItemId(1)]));
}

#[test]
fn warn_shared_names_the_other_member_who_touched_the_file() {
    let mut world = World::new();
    world.claim(1, ALICE, &[]).unwrap();
    world.claim(2, BOB, &[]).unwrap();

    let alice = world.check(ALICE, &["api:config/app.yaml"]);
    assert_eq!(verdict(&alice), &Verdict::Allowed);
    world.apply(&alice);

    let bob = world.check(BOB, &["api:config/app.yaml"]);
    let Verdict::AllowedWithNote { touched_by, .. } = verdict(&bob) else {
        panic!("expected a note, got {:?}", verdict(&bob));
    };
    assert_eq!(touched_by.len(), 1);
    assert_eq!(touched_by[0].member, ALICE);
}

#[test]
fn a_stale_claim_is_taken_over_and_keeps_its_free_areas() {
    let mut world = World::new();
    world.claim(1, BOB, &["api:search"]).unwrap();
    world.now += world.settings.stale_after() + Duration::minutes(5);

    let request = world.request(1, ALICE, &[]);
    let current = world.claims.iter().find(|c| c.item == ItemId(1));
    assert!(matches!(
        decide_claim(current, &world.claims, &request, world.now, &world.settings),
        Err(ClaimRefused::Stale { member, .. }) if member == BOB
    ));
    let takeover =
        decide_takeover(current, &world.claims, &request, world.now, &world.settings).unwrap();
    assert_eq!(takeover.previous.member, BOB);
    assert_eq!(takeover.claim.member, ALICE);
    assert!(takeover.claim.holds(&world.area("api:search")));
    assert!(takeover.dropped_areas.is_empty());
}

#[test]
fn people_cannot_create_items_in_unlisted_repositories_but_the_importer_can() {
    let world = World::new();
    let schemes = RefSchemeRegistry::standard();
    let listed = ExternalRef::parse("gh:Acme/Web#12").unwrap();
    let typo = ExternalRef::parse("gh:acme/wbe#12").unwrap();
    assert_eq!(
        decide_create_item(Some(&listed), Role::Member, &world.settings, &schemes),
        Ok(ItemRepository::Listed(RepoId::parse("web").unwrap()))
    );
    assert!(matches!(
        decide_create_item(Some(&typo), Role::Member, &world.settings, &schemes),
        Err(CreateRefused::UnlistedRepository { .. })
    ));
    assert!(matches!(
        decide_create_item(Some(&typo), Role::Importer, &world.settings, &schemes),
        Ok(ItemRepository::Unlisted(_))
    ));
}

#[test]
fn a_checkout_resolves_its_repository_and_host_paths_stay_out_of_relative_paths() {
    let world = World::new();
    let api = RepoId::parse("api").unwrap();
    let repo = world.settings.checkout_repo(Some(&api)).unwrap();
    let path = RelPath::parse("./packages/billing/src/Invoice.php").unwrap();
    assert_eq!(
        repo.area_for(&path).map(|a| a.name.to_string()),
        Some("billing".to_owned())
    );
    assert!(world.settings.checkout_repo(None).is_err());
    assert!(RelPath::parse("C:\\work\\api\\packages\\billing").is_err());
    assert!(RelPath::parse("c:/work/api/packages/billing").is_err());
}

#[test]
fn decisions_publish_events_in_the_documented_shape() {
    let mut world = World::new();
    let outcome = world.claim(1, ALICE, &[]).unwrap();
    let claim = outcome.claim();
    let claimed = EventKind::Claimed(Claimed {
        item: claim.item,
        member: claim.member,
        session: claim.session,
        host: claim.host.clone(),
        areas: claim.areas.clone(),
    });
    let check = world.check(ALICE, &["api:packages/search/a.php", "api:config/app.yaml"]);
    let reserved = EventKind::Reserved(Reserved {
        item: claim.item,
        member: claim.member,
        areas: check
            .plan
            .add_areas
            .iter()
            .map(|a| a.area.clone())
            .collect(),
        paths: check
            .plan
            .share_paths
            .iter()
            .map(|s| s.path.clone())
            .collect(),
    });
    let json = serde_json::to_value([&claimed, &reserved]).unwrap();
    assert_eq!(
        json,
        serde_json::json!([
            {"kind": "claimed", "payload": {
                "item": 1, "member": 1, "session": 10, "host": "laptop", "areas": []}},
            {"kind": "reserved", "payload": {
                "item": 1, "member": 1, "areas": ["api:search"], "paths": ["api:config/app.yaml"]}}
        ])
    );
}

#[test]
fn settings_are_written_back_in_a_form_that_parses_to_the_same_settings() {
    let world = World::new();
    let written = serde_json::to_string(&world.settings.to_document()).unwrap();
    let document: SettingsDocument = serde_json::from_str(&written).unwrap();
    let again = ProjectSettings::parse(&document, &HotRuleRegistry::standard()).unwrap();
    assert_eq!(again, world.settings);
}

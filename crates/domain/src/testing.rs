//! Fixtures shared by this crate's unit tests.

use std::collections::BTreeSet;

use time::{Duration, OffsetDateTime};

use crate::area::QualifiedArea;
use crate::claim::{Claim, ClaimRequest};
use crate::hot_rule::HotRuleRegistry;
use crate::ids::{ItemId, MemberId, SessionId};
use crate::session::HostName;
use crate::settings::{ProjectSettings, SettingsDocument};

pub(crate) const ALICE: MemberId = MemberId(1);
pub(crate) const BOB: MemberId = MemberId(2);
pub(crate) const CAROL: MemberId = MemberId(3);

/// A fixed instant plus some minutes.
pub(crate) fn at(minutes: i64) -> OffsetDateTime {
    OffsetDateTime::UNIX_EPOCH + Duration::minutes(minutes)
}

pub(crate) fn area(text: &str) -> QualifiedArea {
    QualifiedArea::parse(text).unwrap()
}

pub(crate) fn areas(texts: &[&str]) -> BTreeSet<QualifiedArea> {
    texts.iter().map(|text| area(text)).collect()
}

/// A claim claimed, renewed and last seen `minutes` after the fixed instant.
pub(crate) fn claim(item: u64, member: MemberId, declared: &[&str], minutes: i64) -> Claim {
    Claim {
        item: ItemId(item),
        member,
        session: SessionId(member.get() * 100),
        host: HostName::parse("laptop").unwrap(),
        areas: areas(declared),
        wide: None,
        claimed_at: at(minutes),
        renewed_at: at(minutes),
        last_seen: at(minutes),
        state: crate::review::ClaimState::Working,
        branches: std::collections::BTreeMap::new(),
    }
}

pub(crate) fn request(item: u64, by: MemberId, declared: &[&str]) -> ClaimRequest {
    ClaimRequest {
        item: ItemId(item),
        by,
        session: SessionId(by.get() * 100 + 1),
        host: HostName::parse("desktop").unwrap(),
        areas: areas(declared),
        wide: None,
    }
}

pub(crate) fn document(json: &str) -> SettingsDocument {
    serde_json::from_str(json).unwrap()
}

pub(crate) fn settings(json: &str) -> ProjectSettings {
    ProjectSettings::parse(&document(json), &HotRuleRegistry::standard()).unwrap()
}

/// Two repositories: `api` with areas `billing`, `search` and `billing-docs` (inside billing's
/// globs, declared after it) and hot rules; `web` with `billing` and `checkout`.
pub(crate) fn two_repo_json(policy: &str) -> String {
    format!(
        r#"{{"edit_policy":"{policy}","repos":[
            {{"id":"api","remote":"example.com/acme/api",
              "areas":[{{"name":"billing","paths":["packages/billing/**"]}},
                       {{"name":"search","paths":["packages/search/**"]}},
                       {{"name":"billing-docs","paths":["packages/billing/docs/**"]}}],
              "hot":[{{"paths":["**/*.lock"],"kind":"regenerate","note":"regenerate it; never merge by hand"}},
                     {{"paths":["config/routes.yaml"],"kind":"free"}},
                     {{"paths":["config/**"],"kind":"warn-shared","note":"shared configuration"}},
                     {{"paths":["deploy/billing.yaml"],"kind":"owner","area":"billing"}}]}},
            {{"id":"web","remote":"example.com/acme/web",
              "areas":[{{"name":"billing","paths":["src/billing/**"]}},
                       {{"name":"checkout","paths":["src/checkout/**"]}}]}}
        ]}}"#
    )
}

pub(crate) fn two_repo_settings() -> ProjectSettings {
    settings(&two_repo_json("require-claim"))
}

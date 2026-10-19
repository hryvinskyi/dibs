//! A project for the use cases' tests: an admin, a member and an importer with their tokens and
//! sessions, under settings the test chooses.

use time::{Duration, OffsetDateTime};

use dibs_domain::event::{Event, EventKind};
use dibs_domain::handle::Handle;
use dibs_domain::ids::{ItemId, ProjectId, SessionId};
use dibs_domain::item::{ExternalRef, Title};
use dibs_domain::member::Role;
use dibs_domain::message::SessionLabel;
use dibs_domain::session::HostName;
use dibs_domain::settings::SettingsDocument;

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::fakes::Fakes;
use crate::use_cases::{
    ApplySettings, Authenticate, CreateItem, CreateItemInput, CreateMember, CreateMemberInput,
    InitProject, InitProjectInput, NewMember, RegisterSession, RegisterSessionInput,
};
use dibs_domain::member::DisplayName;
use dibs_domain::project::ProjectSlug;
use dibs_domain::session::AgentLabel;

/// The instant every test starts at.
pub(crate) const START: OffsetDateTime = OffsetDateTime::UNIX_EPOCH;

/// Two areas of a single-repository project, and two hot-file rules.
pub(crate) const SETTINGS: &str = r#"{
    "stale_after": "24h",
    "edit_policy": "require-claim",
    "areas": [
        {"name": "billing", "paths": ["packages/billing/**"]},
        {"name": "search", "paths": ["packages/search/**"]}
    ],
    "hot": [
        {"paths": ["**/*.lock"], "kind": "regenerate", "note": "regenerate it"},
        {"paths": ["config/**"], "kind": "warn-shared"}
    ]
}"#;

pub(crate) fn new_member(handle: &str) -> NewMember {
    NewMember {
        handle: Handle::parse(handle).unwrap(),
        display_name: DisplayName::parse(&handle.to_uppercase()).unwrap(),
        external_login: None,
    }
}

pub(crate) fn document(json: &str) -> SettingsDocument {
    serde_json::from_str(json).unwrap()
}

/// A member with one session.
pub(crate) struct Person {
    pub(crate) actor: Actor,
    pub(crate) session: SessionId,
}

/// The project as the tests see it.
pub(crate) struct Env {
    pub(crate) fakes: Fakes,
    pub(crate) deps: Dependencies,
    pub(crate) project: ProjectId,
    pub(crate) alice: Person,
    pub(crate) bob: Person,
    pub(crate) importer: Actor,
}

impl Env {
    /// A project whose settings are the `SETTINGS` document.
    pub(crate) async fn new() -> Self {
        Self::with_settings(SETTINGS).await
    }

    /// A project with an admin `alice`, a member `bob` (each with a session on their own machine)
    /// and an importer, under these settings.
    pub(crate) async fn with_settings(json: &str) -> Self {
        let fakes = Fakes::new(START);
        let deps = fakes.dependencies();
        let created = InitProject::new(&deps)
            .execute(InitProjectInput {
                slug: ProjectSlug::parse("widgets").unwrap(),
                admin: new_member("alice"),
            })
            .await
            .unwrap();
        let authenticate = Authenticate::new(&deps);
        let alice = authenticate.execute(created.token.expose()).await.unwrap();
        ApplySettings::new(&deps)
            .execute(&alice, document(json))
            .await
            .unwrap();
        let create = CreateMember::new(&deps);
        let bob_token = create
            .execute(
                &alice,
                CreateMemberInput {
                    member: new_member("bob"),
                    role: Role::Member,
                },
            )
            .await
            .unwrap()
            .token;
        let importer_token = create
            .execute(
                &alice,
                CreateMemberInput {
                    member: new_member("importer"),
                    role: Role::Importer,
                },
            )
            .await
            .unwrap()
            .token;
        let bob = authenticate.execute(bob_token.expose()).await.unwrap();
        let importer = authenticate.execute(importer_token.expose()).await.unwrap();
        let alice_session = session(&deps, &alice, "laptop", "laptop/4f2a91c0").await;
        let bob_session = session(&deps, &bob, "desktop", "desktop/77aa01bc").await;
        Self {
            project: created.project.id,
            fakes,
            deps,
            alice: Person {
                actor: alice,
                session: alice_session,
            },
            bob: Person {
                actor: bob,
                session: bob_session,
            },
            importer,
        }
    }

    /// Creates an item with this reference and title, as alice.
    pub(crate) async fn item(&self, reference: &str) -> ItemId {
        CreateItem::new(&self.deps)
            .execute(
                &self.alice.actor,
                CreateItemInput {
                    external_ref: Some(ExternalRef::parse(reference).unwrap()),
                    title: Title::parse(&format!("Work on {reference}")).unwrap(),
                },
            )
            .await
            .unwrap()
            .item
            .item
            .id
    }

    /// The project's events so far.
    pub(crate) async fn events(&self) -> Vec<Event> {
        self.fakes
            .store
            .all_events()
            .await
            .into_iter()
            .filter(|event| event.project == self.project)
            .collect()
    }

    /// The kinds of the project's events appended after the first `skip`.
    pub(crate) async fn kinds_after(&self, skip: usize) -> Vec<EventKind> {
        self.events()
            .await
            .into_iter()
            .skip(skip)
            .map(|event| event.kind)
            .collect()
    }

    /// Moves the clock.
    pub(crate) fn advance(&self, by: Duration) {
        self.fakes.clock.advance(by);
    }

    /// The clock's time.
    pub(crate) fn now(&self) -> OffsetDateTime {
        crate::ports::Clock::now(&*self.deps.clock)
    }
}

/// Registers a session and answers its id.
pub(crate) async fn session(
    deps: &Dependencies,
    actor: &Actor,
    host: &str,
    label: &str,
) -> SessionId {
    RegisterSession::new(deps)
        .execute(
            actor,
            RegisterSessionInput {
                host: HostName::parse(host).unwrap(),
                label: SessionLabel::parse(label).unwrap(),
                agent: AgentLabel::parse("agent").unwrap(),
                push: false,
            },
        )
        .await
        .unwrap()
        .id
}

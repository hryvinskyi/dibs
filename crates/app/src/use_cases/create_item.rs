//! Creating a work item, or finding the one its reference already names.

use std::sync::Arc;

use dibs_domain::event::{EventKind, ItemCreated};
use dibs_domain::hot_rule::HotRuleRegistry;
use dibs_domain::ids::ItemId;
use dibs_domain::item::{ExternalRef, Title, decide_create_item};
use dibs_domain::ref_scheme::RefSchemeRegistry;

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::named::{NamedItem, Names};
use crate::ports::{Clock, EventNotifier, IdGen, Store};
use crate::records::ItemRecord;
use crate::work::Work;

/// The item to create.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateItemInput {
    /// Its reference in an outside tracker; an item without one is local.
    pub external_ref: Option<ExternalRef>,
    /// Its title.
    pub title: Title,
}

/// The item the reference names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedItem {
    /// The item, with its local id and its creator's handle.
    pub item: NamedItem,
    /// Whether this call created it, rather than finding it.
    pub created: bool,
}

/// Creates a work item. Idempotent on the reference: an item that exists with the same reference is
/// returned as it is, so a client resolves a reference to its local id by creating it. A person's
/// item in a repository the project does not list is refused; the importer's is accepted.
pub struct CreateItem {
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGen>,
    notifier: Arc<dyn EventNotifier>,
    hot_rules: Arc<HotRuleRegistry>,
    ref_schemes: Arc<RefSchemeRegistry>,
}

impl CreateItem {
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

    /// Creates the item, or finds it.
    ///
    /// # Errors
    /// [`AppError::Create`] for a person naming an unlisted repository; [`AppError::Store`] when the
    /// store fails.
    pub async fn execute(
        &self,
        actor: &Actor,
        input: CreateItemInput,
    ) -> Result<CreatedItem, AppError> {
        let now = self.clock.now();
        let mut work = Work::begin(&*self.store, actor.project).await?;
        if let Some(reference) = &input.external_ref
            && let Some(item) = work.item_by_ref(actor.project, reference).await?
        {
            let item = Names::load(&mut work).await?.item(item)?;
            return Ok(CreatedItem {
                item,
                created: false,
            });
        }
        let settings = work.project_settings(&self.hot_rules).await?;
        let repository = decide_create_item(
            input.external_ref.as_ref(),
            actor.role,
            &settings,
            &self.ref_schemes,
        )?;
        let item = ItemRecord::new(
            ItemId(self.ids.next()),
            input.external_ref,
            input.title,
            repository,
            actor.member,
            now,
        );
        work.insert_item(actor.project, &item).await?;
        work.record(
            now,
            EventKind::ItemCreated(ItemCreated {
                item: item.id,
                external_ref: item.external_ref.clone(),
                title: item.title.clone(),
                by: actor.member,
            }),
        )
        .await?;
        let item = Names::load(&mut work).await?.item(item)?;
        work.commit(&*self.notifier).await?;
        Ok(CreatedItem {
            item,
            created: true,
        })
    }
}

#[cfg(test)]
mod tests {
    use dibs_domain::item::{ItemRepository, ItemStatus};
    use dibs_domain::repo::{RemoteUrl, RepoId};

    use super::*;
    use crate::use_cases::testing::Env;

    const TWO_REPOS: &str = r#"{"repos":[
        {"id":"api","remote":"https://github.com/acme/api.git",
         "areas":[{"name":"billing","paths":["src/billing/**"]}]},
        {"id":"web","remote":"git@github.com:acme/web.git"}
    ]}"#;

    fn input(reference: Option<&str>, title: &str) -> CreateItemInput {
        CreateItemInput {
            external_ref: reference.map(|r| ExternalRef::parse(r).unwrap()),
            title: Title::parse(title).unwrap(),
        }
    }

    #[tokio::test]
    async fn an_item_is_created_open_and_announced() {
        let env = Env::new().await;
        let before = env.events().await.len();
        let created = CreateItem::new(&env.deps)
            .execute(
                &env.bob.actor,
                input(Some("gh:acme/widgets#42"), "Fix the invoice total"),
            )
            .await
            .unwrap();
        assert!(created.created);
        assert_eq!(created.item.item.status, ItemStatus::Open);
        assert_eq!(created.item.item.created_by, env.bob.actor.member);
        assert_eq!(created.item.created_by.as_str(), "bob");
        assert_eq!(
            env.kinds_after(before).await,
            [EventKind::ItemCreated(ItemCreated {
                item: created.item.item.id,
                external_ref: created.item.item.external_ref.clone(),
                title: created.item.item.title.clone(),
                by: env.bob.actor.member,
            })]
        );
        let last = env.events().await.last().unwrap().id;
        assert_eq!(
            env.fakes.notifier.calls().last(),
            Some(&(env.project, last))
        );
    }

    #[tokio::test]
    async fn the_same_reference_finds_the_same_item_and_appends_nothing() {
        let env = Env::new().await;
        let create = CreateItem::new(&env.deps);
        let first = create
            .execute(&env.bob.actor, input(Some("gh:acme/widgets#42"), "One"))
            .await
            .unwrap();
        let events = env.events().await.len();
        let again = create
            .execute(&env.alice.actor, input(Some("gh:acme/widgets#42"), "Two"))
            .await
            .unwrap();
        assert!(!again.created);
        assert_eq!(again.item, first.item);
        assert_eq!(env.events().await.len(), events);
    }

    #[tokio::test]
    async fn local_items_are_always_new() {
        let env = Env::new().await;
        let create = CreateItem::new(&env.deps);
        let a = create
            .execute(&env.bob.actor, input(None, "A"))
            .await
            .unwrap();
        let b = create
            .execute(&env.bob.actor, input(None, "A"))
            .await
            .unwrap();
        assert_ne!(a.item.item.id, b.item.item.id);
        assert_eq!(a.item.item.repository, ItemRepository::Unnamed);
    }

    #[tokio::test]
    async fn a_reference_names_its_listed_repository() {
        let env = Env::with_settings(TWO_REPOS).await;
        let created = CreateItem::new(&env.deps)
            .execute(&env.bob.actor, input(Some("gh:acme/web#12"), "Checkout"))
            .await
            .unwrap();
        assert_eq!(
            created.item.item.repository,
            ItemRepository::Listed(RepoId::parse("web").unwrap())
        );
    }

    #[tokio::test]
    async fn a_person_is_refused_an_unlisted_repository_and_the_importer_is_not() {
        let env = Env::with_settings(TWO_REPOS).await;
        let create = CreateItem::new(&env.deps);
        let before = env.events().await.len();
        let refused = create
            .execute(&env.bob.actor, input(Some("gh:acme/serch#3"), "Typo"))
            .await
            .unwrap_err();
        assert_eq!(refused.code(), "unlisted_repository");
        assert_eq!(env.events().await.len(), before);
        let imported = create
            .execute(&env.importer, input(Some("gh:acme/search#3"), "Blocker"))
            .await
            .unwrap();
        assert_eq!(
            imported.item.item.repository,
            ItemRepository::Unlisted(RemoteUrl::parse("github.com/acme/search").unwrap())
        );
    }
}

//! The store's contract, case by case, run against the in-memory store the use cases are specified
//! with and against the SQLite store on a real file in a temporary directory. Every case runs on
//! both, so a behaviour one of them changes alone shows up as a failing case of the other.

#[cfg(test)]
mod contract {
    use std::collections::BTreeSet;
    use std::sync::Arc;

    use tempfile::TempDir;
    use time::{Duration, OffsetDateTime};

    use dibs_app::fakes::MemoryStore;
    use dibs_app::ports::{Store, StoreError};
    use dibs_app::records::{
        GrantRecord, InProject, ItemFilter, ItemRecord, MemberRecord, MessageRecord, ProjectRecord,
        SessionRecord, ThreadRecord, TokenRecord, UiSessionRecord,
    };
    use dibs_app::token::{TokenHash, TokenPrefix};
    use dibs_domain::area::QualifiedArea;
    use dibs_domain::claim::Claim;
    use dibs_domain::event::{
        Claimed, Event, EventKind, GrantEnd, GrantRevoked, ItemCreated, MessagePosted, Reserved,
        SettingsChanged,
    };
    use dibs_domain::grant::Grant;
    use dibs_domain::handle::Handle;
    use dibs_domain::ids::{
        EventId, GrantId, ItemId, MemberId, MessageId, ProjectId, SessionId, ThreadId,
    };
    use dibs_domain::item::{ExternalRef, ItemRepository, ItemStatus, Title};
    use dibs_domain::member::{DisplayName, ExternalLogin, Role};
    use dibs_domain::message::{Body, Recipients, SessionLabel, Subject};
    use dibs_domain::paths::RepoPath;
    use dibs_domain::project::ProjectSlug;
    use dibs_domain::repo::{RemoteUrl, RepoId};
    use dibs_domain::reservation::SharedPath;
    use dibs_domain::session::{AgentLabel, HostName};
    use dibs_domain::settings::SettingsDocument;
    use dibs_store_sqlite::SqliteStore;

    /// A store under test, with the directory its file lives in.
    struct UnderTest {
        store: Arc<dyn Store>,
        _dir: Option<TempDir>,
    }

    fn memory() -> UnderTest {
        UnderTest {
            store: Arc::new(MemoryStore::default()),
            _dir: None,
        }
    }

    async fn sqlite() -> UnderTest {
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteStore::open(&dir.path().join("dibs.db"))
            .await
            .unwrap();
        UnderTest {
            store: Arc::new(store),
            _dir: Some(dir),
        }
    }

    /// One test per case and store.
    macro_rules! contract {
        ($($case:ident),* $(,)?) => {
            mod memory_store {
                $(
                    #[tokio::test]
                    async fn $case() {
                        super::$case(super::memory().store).await;
                    }
                )*
            }

            mod sqlite_store {
                $(
                    #[tokio::test]
                    async fn $case() {
                        let subject = super::sqlite().await;
                        super::$case(subject.store.clone()).await;
                    }
                )*
            }
        };
    }

    contract!(
        uncommitted_work_is_discarded,
        a_transaction_waits_for_the_one_before_it,
        a_refused_duplicate_leaves_the_transaction_usable,
        projects_are_unique_by_id_and_by_slug,
        members_are_scoped_to_their_project,
        tokens_are_found_by_hash_anywhere_and_by_prefix_in_their_project,
        a_revoked_token_keeps_its_first_revocation,
        sessions_are_replaced_by_id_and_unique_by_label_per_member,
        items_are_unique_by_reference_and_filtered,
        items_keep_the_repository_they_belong_to,
        claims_are_kept_live_or_stale_and_replaced_per_item,
        shared_paths_are_kept_once_and_removed_explicitly,
        grants_are_listed_by_id,
        threads_keep_their_recipients_and_messages_their_order,
        summaries_count_what_the_reader_has_not_read,
        summaries_put_the_newest_activity_first_and_filter_by_item,
        events_only_increase_and_page_by_cursor,
        events_keep_every_field,
        settings_are_replaced_whole,
        browser_sessions_are_found_by_hash_and_swept_when_ended,
        a_reference_to_a_missing_record_fails_the_commit_and_writes_nothing,
        the_largest_id_the_port_allows_is_kept,
    );

    const A: ProjectId = ProjectId(1);
    const B: ProjectId = ProjectId(2);
    const ALICE: MemberId = MemberId(11);
    const BOB: MemberId = MemberId(12);
    const DAVE: MemberId = MemberId(21);
    const ALICE_LAPTOP: SessionId = SessionId(31);
    const BOB_LAPTOP: SessionId = SessionId(32);
    const DAVE_LAPTOP: SessionId = SessionId(41);
    const FIRST: ItemId = ItemId(51);
    const SECOND: ItemId = ItemId(52);
    const THIRD: ItemId = ItemId(53);
    const GADGET: ItemId = ItemId(61);

    /// `seconds` after the epoch, plus a fraction that only a store keeping nanoseconds returns.
    fn at(seconds: i64) -> OffsetDateTime {
        OffsetDateTime::UNIX_EPOCH + Duration::seconds(seconds) + Duration::nanoseconds(123_456_789)
    }

    fn project(id: ProjectId, slug: &str) -> ProjectRecord {
        ProjectRecord {
            id,
            slug: ProjectSlug::parse(slug).unwrap(),
            created_at: at(0),
        }
    }

    fn member(id: MemberId, handle: &str) -> MemberRecord {
        MemberRecord {
            id,
            handle: Handle::parse(handle).unwrap(),
            display_name: DisplayName::parse(&handle.to_uppercase()).unwrap(),
            external_login: None,
            role: Role::Member,
            created_at: at(1),
        }
    }

    fn token(hash: &str, prefix: &str, member: MemberId, created: i64) -> TokenRecord {
        TokenRecord {
            hash: TokenHash::new(hash.to_owned()),
            prefix: TokenPrefix::parse(prefix).unwrap(),
            member,
            created_at: at(created),
            revoked_at: None,
        }
    }

    fn session(id: SessionId, member: MemberId, label: &str) -> SessionRecord {
        SessionRecord {
            id,
            member,
            host: HostName::parse("laptop").unwrap(),
            label: SessionLabel::parse(label).unwrap(),
            agent: AgentLabel::parse("agent").unwrap(),
            push: false,
            last_seen: at(2),
        }
    }

    fn item(id: ItemId, reference: Option<&str>, by: MemberId) -> ItemRecord {
        ItemRecord {
            id,
            external_ref: reference.map(|r| ExternalRef::parse(r).unwrap()),
            title: Title::parse("Fix the invoice totals").unwrap(),
            status: ItemStatus::Open,
            repository: ItemRepository::Unnamed,
            created_by: by,
            created_at: at(3),
        }
    }

    fn areas(names: &[&str]) -> BTreeSet<QualifiedArea> {
        names
            .iter()
            .map(|n| QualifiedArea::parse(n).unwrap())
            .collect()
    }

    fn claim(item: ItemId, member: MemberId, session: SessionId, names: &[&str]) -> Claim {
        Claim {
            item,
            member,
            session,
            host: HostName::parse("laptop").unwrap(),
            areas: areas(names),
            claimed_at: at(10),
            renewed_at: at(11),
            last_seen: at(12),
        }
    }

    fn shared(item: ItemId, path: &str) -> SharedPath {
        SharedPath {
            claim: item,
            path: RepoPath::parse(path).unwrap(),
        }
    }

    fn thread(id: ThreadId, author: MemberId, item: Option<ItemId>, created: i64) -> ThreadRecord {
        ThreadRecord {
            id,
            subject: Subject::parse("Invoice totals").unwrap(),
            item,
            author,
            recipients: Recipients::Project,
            created_at: at(created),
        }
    }

    fn message(id: u64, thread: ThreadId, author: MemberId, created: i64) -> MessageRecord {
        MessageRecord {
            id: MessageId(id),
            thread,
            author,
            label: SessionLabel::parse("laptop/mcp").unwrap(),
            body: Body::parse("Can I take billing for an hour?\nThanks.").unwrap(),
            created_at: at(created),
        }
    }

    /// Two projects: `widgets` with alice and bob, a session each and three items; `gadgets` with
    /// dave, his session and one item.
    async fn seed(store: &Arc<dyn Store>) {
        let mut tx = store.begin().await.unwrap();
        tx.insert_project(&project(A, "widgets")).await.unwrap();
        tx.insert_project(&project(B, "gadgets")).await.unwrap();
        tx.insert_member(A, &member(ALICE, "alice")).await.unwrap();
        tx.insert_member(A, &member(BOB, "bob")).await.unwrap();
        tx.insert_member(B, &member(DAVE, "dave")).await.unwrap();
        tx.save_session(A, &session(ALICE_LAPTOP, ALICE, "laptop/mcp"))
            .await
            .unwrap();
        tx.save_session(A, &session(BOB_LAPTOP, BOB, "laptop/mcp"))
            .await
            .unwrap();
        tx.save_session(B, &session(DAVE_LAPTOP, DAVE, "laptop/mcp"))
            .await
            .unwrap();
        tx.insert_item(A, &item(FIRST, Some("gh:acme/widgets#1"), ALICE))
            .await
            .unwrap();
        tx.insert_item(A, &item(SECOND, None, ALICE)).await.unwrap();
        tx.insert_item(A, &item(THIRD, Some("gh:acme/widgets#3"), BOB))
            .await
            .unwrap();
        tx.insert_item(B, &item(GADGET, Some("gh:acme/widgets#1"), DAVE))
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }

    fn is_duplicate<T: std::fmt::Debug>(result: Result<T, StoreError>) -> bool {
        matches!(result, Err(StoreError::Duplicate))
    }

    async fn uncommitted_work_is_discarded(store: Arc<dyn Store>) {
        let mut tx = store.begin().await.unwrap();
        tx.insert_project(&project(A, "widgets")).await.unwrap();
        tx.append_event(
            A,
            at(0),
            &EventKind::SettingsChanged(SettingsChanged { by: ALICE }),
        )
        .await
        .unwrap();
        assert!(tx.project(A).await.unwrap().is_some());
        drop(tx);

        let mut tx = store.begin().await.unwrap();
        assert_eq!(tx.project(A).await.unwrap(), None);
        assert_eq!(tx.latest_event(A).await.unwrap(), None);
    }

    async fn a_transaction_waits_for_the_one_before_it(store: Arc<dyn Store>) {
        let mut first = store.begin().await.unwrap();
        first.insert_project(&project(A, "widgets")).await.unwrap();

        let second = tokio::spawn({
            let store = store.clone();
            async move {
                let mut tx = store.begin().await.unwrap();
                let seen = tx.project(A).await.unwrap();
                tx.insert_project(&project(B, "gadgets")).await.unwrap();
                tx.commit().await.unwrap();
                seen
            }
        });
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert!(!second.is_finished());

        first.commit().await.unwrap();
        let seen = second.await.unwrap();
        assert_eq!(seen, Some(project(A, "widgets")));
        let mut tx = store.begin().await.unwrap();
        assert!(tx.project(B).await.unwrap().is_some());
    }

    async fn a_refused_duplicate_leaves_the_transaction_usable(store: Arc<dyn Store>) {
        let mut tx = store.begin().await.unwrap();
        tx.insert_project(&project(A, "widgets")).await.unwrap();
        assert!(is_duplicate(
            tx.insert_project(&project(A, "gadgets")).await
        ));
        tx.insert_project(&project(B, "gadgets")).await.unwrap();
        tx.commit().await.unwrap();

        let mut tx = store.begin().await.unwrap();
        assert_eq!(tx.project(A).await.unwrap(), Some(project(A, "widgets")));
        assert_eq!(tx.project(B).await.unwrap(), Some(project(B, "gadgets")));
    }

    async fn projects_are_unique_by_id_and_by_slug(store: Arc<dyn Store>) {
        seed(&store).await;
        let mut tx = store.begin().await.unwrap();
        assert_eq!(tx.project(A).await.unwrap(), Some(project(A, "widgets")));
        let slug = ProjectSlug::parse("gadgets").unwrap();
        assert_eq!(
            tx.project_by_slug(&slug).await.unwrap(),
            Some(project(B, "gadgets"))
        );
        assert_eq!(tx.project(ProjectId(9)).await.unwrap(), None);
        let unknown = ProjectSlug::parse("nothing").unwrap();
        assert_eq!(tx.project_by_slug(&unknown).await.unwrap(), None);
        assert!(is_duplicate(tx.insert_project(&project(A, "fresh")).await));
        assert!(is_duplicate(
            tx.insert_project(&project(ProjectId(9), "widgets")).await
        ));
    }

    async fn members_are_scoped_to_their_project(store: Arc<dyn Store>) {
        seed(&store).await;
        let mut tx = store.begin().await.unwrap();
        let carol = MemberRecord {
            external_login: Some(ExternalLogin::parse("carol-gh").unwrap()),
            role: Role::Admin,
            ..member(MemberId(13), "carol")
        };
        tx.insert_member(A, &carol).await.unwrap();
        let importer = MemberRecord {
            role: Role::Importer,
            ..member(MemberId(14), "aaa-importer")
        };
        tx.insert_member(A, &importer).await.unwrap();
        tx.commit().await.unwrap();

        let mut tx = store.begin().await.unwrap();
        assert_eq!(
            tx.member(A, MemberId(13)).await.unwrap(),
            Some(carol.clone())
        );
        let handle = Handle::parse("carol").unwrap();
        assert_eq!(tx.member_by_handle(A, &handle).await.unwrap(), Some(carol));
        let handles: Vec<String> = tx
            .members(A)
            .await
            .unwrap()
            .into_iter()
            .map(|m| m.handle.to_string())
            .collect();
        assert_eq!(handles, ["aaa-importer", "alice", "bob", "carol"]);

        assert_eq!(tx.member(B, ALICE).await.unwrap(), None);
        assert_eq!(tx.member_by_handle(B, &handle).await.unwrap(), None);
        assert_eq!(tx.members(B).await.unwrap(), vec![member(DAVE, "dave")]);

        assert!(is_duplicate(
            tx.insert_member(A, &member(MemberId(15), "alice")).await
        ));
        assert!(is_duplicate(
            tx.insert_member(A, &member(ALICE, "alicia")).await
        ));
        tx.insert_member(B, &member(MemberId(22), "alice"))
            .await
            .unwrap();
    }

    async fn tokens_are_found_by_hash_anywhere_and_by_prefix_in_their_project(
        store: Arc<dyn Store>,
    ) {
        seed(&store).await;
        let mut tx = store.begin().await.unwrap();
        let newer = token("hash:b", "AAAAAAAB", ALICE, 20);
        let older = token("hash:c", "AAAAAAAC", ALICE, 10);
        let same_time = token("hash:a", "AAAAAAAA", ALICE, 20);
        for record in [&newer, &older, &same_time] {
            tx.insert_token(A, record).await.unwrap();
        }
        tx.insert_token(A, &token("hash:bob", "BBBBBBBB", BOB, 5))
            .await
            .unwrap();
        tx.insert_token(B, &token("hash:dave", "AAAAAAAA", DAVE, 5))
            .await
            .unwrap();
        tx.commit().await.unwrap();

        let mut tx = store.begin().await.unwrap();
        assert_eq!(
            tx.token_by_hash(&TokenHash::new("hash:dave".to_owned()))
                .await
                .unwrap(),
            Some(InProject {
                project: B,
                record: token("hash:dave", "AAAAAAAA", DAVE, 5),
            })
        );
        assert_eq!(
            tx.token_by_hash(&TokenHash::new("hash:none".to_owned()))
                .await
                .unwrap(),
            None
        );
        let prefix = TokenPrefix::parse("AAAAAAAC").unwrap();
        assert_eq!(
            tx.token_by_prefix(A, &prefix).await.unwrap(),
            Some(older.clone())
        );
        assert_eq!(tx.token_by_prefix(B, &prefix).await.unwrap(), None);
        assert_eq!(
            tx.tokens_of(A, ALICE).await.unwrap(),
            vec![older, same_time, newer]
        );
        assert!(tx.tokens_of(B, ALICE).await.unwrap().is_empty());

        assert!(is_duplicate(
            tx.insert_token(B, &token("hash:a", "CCCCCCCC", DAVE, 5))
                .await
        ));
        assert!(is_duplicate(
            tx.insert_token(A, &token("hash:new", "BBBBBBBB", ALICE, 5))
                .await
        ));
    }

    async fn a_revoked_token_keeps_its_first_revocation(store: Arc<dyn Store>) {
        seed(&store).await;
        let hash = TokenHash::new("hash:a".to_owned());
        let mut tx = store.begin().await.unwrap();
        tx.insert_token(A, &token("hash:a", "AAAAAAAA", ALICE, 1))
            .await
            .unwrap();
        tx.revoke_token(B, &hash, at(5)).await.unwrap();
        assert_eq!(tx.tokens_of(A, ALICE).await.unwrap()[0].revoked_at, None);
        tx.revoke_token(A, &hash, at(10)).await.unwrap();
        tx.commit().await.unwrap();

        let mut tx = store.begin().await.unwrap();
        tx.revoke_token(A, &hash, at(20)).await.unwrap();
        let stored = tx.token_by_hash(&hash).await.unwrap().unwrap().record;
        assert_eq!(stored.revoked_at, Some(at(10)));
        assert!(!stored.is_active());
        tx.revoke_token(A, &TokenHash::new("hash:none".to_owned()), at(20))
            .await
            .unwrap();
    }

    async fn sessions_are_replaced_by_id_and_unique_by_label_per_member(store: Arc<dyn Store>) {
        seed(&store).await;
        let mut tx = store.begin().await.unwrap();
        let moved = SessionRecord {
            host: HostName::parse("desktop").unwrap(),
            label: SessionLabel::parse("desktop/mcp").unwrap(),
            agent: AgentLabel::parse("other-agent").unwrap(),
            push: true,
            last_seen: at(30),
            ..session(ALICE_LAPTOP, ALICE, "ignored")
        };
        tx.save_session(A, &moved).await.unwrap();
        tx.commit().await.unwrap();

        let mut tx = store.begin().await.unwrap();
        assert_eq!(
            tx.session(A, ALICE_LAPTOP).await.unwrap(),
            Some(moved.clone())
        );
        let desktop = SessionLabel::parse("desktop/mcp").unwrap();
        let laptop = SessionLabel::parse("laptop/mcp").unwrap();
        assert_eq!(
            tx.session_by_label(A, ALICE, &desktop).await.unwrap(),
            Some(moved)
        );
        assert_eq!(tx.session_by_label(A, ALICE, &laptop).await.unwrap(), None);
        assert_eq!(
            tx.session_by_label(A, BOB, &laptop).await.unwrap(),
            Some(session(BOB_LAPTOP, BOB, "laptop/mcp"))
        );
        assert_eq!(tx.session(B, ALICE_LAPTOP).await.unwrap(), None);
        assert_eq!(tx.session_by_label(B, BOB, &laptop).await.unwrap(), None);

        assert!(is_duplicate(
            tx.save_session(A, &session(SessionId(33), BOB, "laptop/mcp"))
                .await
        ));
        tx.save_session(A, &session(SessionId(34), ALICE, "laptop/mcp"))
            .await
            .unwrap();
    }

    async fn items_are_unique_by_reference_and_filtered(store: Arc<dyn Store>) {
        seed(&store).await;
        let mut tx = store.begin().await.unwrap();
        tx.set_item_status(A, THIRD, ItemStatus::Claimed)
            .await
            .unwrap();
        tx.set_item_status(B, FIRST, ItemStatus::Dropped)
            .await
            .unwrap();
        tx.set_item_status(A, ItemId(99), ItemStatus::Done)
            .await
            .unwrap();
        tx.save_claim(A, &claim(THIRD, BOB, BOB_LAPTOP, &[]))
            .await
            .unwrap();
        tx.commit().await.unwrap();

        let mut tx = store.begin().await.unwrap();
        let reference = ExternalRef::parse("gh:acme/widgets#1").unwrap();
        let first = item(FIRST, Some("gh:acme/widgets#1"), ALICE);
        assert_eq!(tx.item(A, FIRST).await.unwrap(), Some(first.clone()));
        assert_eq!(tx.item_by_ref(A, &reference).await.unwrap(), Some(first));
        assert_eq!(
            tx.item_by_ref(B, &reference).await.unwrap().map(|i| i.id),
            Some(GADGET)
        );
        assert_eq!(tx.item(B, FIRST).await.unwrap(), None);

        let ids = |items: Vec<ItemRecord>| items.into_iter().map(|i| i.id).collect::<Vec<_>>();
        let all = ItemFilter::default();
        assert_eq!(
            ids(tx.items(A, &all).await.unwrap()),
            [FIRST, SECOND, THIRD]
        );
        assert_eq!(ids(tx.items(B, &all).await.unwrap()), [GADGET]);
        let open = ItemFilter {
            status: Some(ItemStatus::Open),
            ..ItemFilter::default()
        };
        assert_eq!(ids(tx.items(A, &open).await.unwrap()), [FIRST, SECOND]);
        let bobs = ItemFilter {
            holder: Some(BOB),
            ..ItemFilter::default()
        };
        assert_eq!(ids(tx.items(A, &bobs).await.unwrap()), [THIRD]);
        assert!(tx.items(B, &bobs).await.unwrap().is_empty());
        let by_ref = ItemFilter {
            external_ref: Some(reference),
            ..ItemFilter::default()
        };
        assert_eq!(ids(tx.items(A, &by_ref).await.unwrap()), [FIRST]);
        let none_match = ItemFilter {
            status: Some(ItemStatus::Claimed),
            ..by_ref
        };
        assert!(tx.items(A, &none_match).await.unwrap().is_empty());

        assert!(is_duplicate(
            tx.insert_item(A, &item(ItemId(54), Some("gh:acme/widgets#1"), ALICE))
                .await
        ));
        assert!(is_duplicate(
            tx.insert_item(A, &item(FIRST, Some("gh:acme/widgets#9"), ALICE))
                .await
        ));
        tx.insert_item(A, &item(ItemId(55), None, ALICE))
            .await
            .unwrap();
    }

    async fn items_keep_the_repository_they_belong_to(store: Arc<dyn Store>) {
        seed(&store).await;
        let listed = ItemRecord {
            repository: ItemRepository::Listed(RepoId::parse("api").unwrap()),
            ..item(ItemId(56), Some("gh:acme/api#7"), ALICE)
        };
        let unlisted = ItemRecord {
            repository: ItemRepository::Unlisted(
                RemoteUrl::parse("git@example.com:acme/search.git").unwrap(),
            ),
            status: ItemStatus::Done,
            ..item(ItemId(57), Some("gh:acme/search#8"), ALICE)
        };
        let mut tx = store.begin().await.unwrap();
        tx.insert_item(A, &listed).await.unwrap();
        tx.insert_item(A, &unlisted).await.unwrap();
        tx.commit().await.unwrap();

        let mut tx = store.begin().await.unwrap();
        assert_eq!(tx.item(A, ItemId(56)).await.unwrap(), Some(listed));
        assert_eq!(tx.item(A, ItemId(57)).await.unwrap(), Some(unlisted));
        assert_eq!(
            tx.item(A, SECOND).await.unwrap().map(|i| i.repository),
            Some(ItemRepository::Unnamed)
        );
    }

    async fn claims_are_kept_live_or_stale_and_replaced_per_item(store: Arc<dyn Store>) {
        seed(&store).await;
        let mut tx = store.begin().await.unwrap();
        let third = claim(THIRD, BOB, BOB_LAPTOP, &["api:billing", "web:checkout"]);
        let stale = Claim {
            last_seen: OffsetDateTime::UNIX_EPOCH,
            ..claim(FIRST, ALICE, ALICE_LAPTOP, &["default:search"])
        };
        tx.save_claim(A, &third).await.unwrap();
        tx.save_claim(A, &stale).await.unwrap();
        tx.save_claim(B, &claim(GADGET, DAVE, DAVE_LAPTOP, &["default:search"]))
            .await
            .unwrap();
        tx.commit().await.unwrap();

        let mut tx = store.begin().await.unwrap();
        assert_eq!(
            tx.claims(A).await.unwrap(),
            vec![stale.clone(), third.clone()]
        );
        assert_eq!(tx.claim(A, THIRD).await.unwrap(), Some(third));
        assert_eq!(tx.claim(B, THIRD).await.unwrap(), None);
        assert_eq!(tx.claim(A, SECOND).await.unwrap(), None);

        let taken = Claim {
            member: BOB,
            session: BOB_LAPTOP,
            areas: BTreeSet::new(),
            claimed_at: at(40),
            renewed_at: at(40),
            last_seen: at(40),
            ..stale
        };
        tx.save_claim(A, &taken).await.unwrap();
        assert_eq!(tx.claim(A, FIRST).await.unwrap(), Some(taken));
        tx.delete_claim(A, THIRD).await.unwrap();
        tx.delete_claim(B, FIRST).await.unwrap();
        tx.commit().await.unwrap();

        let mut tx = store.begin().await.unwrap();
        let items: Vec<ItemId> = tx.claims(A).await.unwrap().iter().map(|c| c.item).collect();
        assert_eq!(items, [FIRST]);
        assert_eq!(tx.claims(B).await.unwrap().len(), 1);
    }

    async fn shared_paths_are_kept_once_and_removed_explicitly(store: Arc<dyn Store>) {
        seed(&store).await;
        let mut tx = store.begin().await.unwrap();
        tx.save_claim(A, &claim(FIRST, ALICE, ALICE_LAPTOP, &[]))
            .await
            .unwrap();
        tx.save_claim(A, &claim(THIRD, BOB, BOB_LAPTOP, &[]))
            .await
            .unwrap();
        tx.save_claim(B, &claim(GADGET, DAVE, DAVE_LAPTOP, &[]))
            .await
            .unwrap();
        for entry in [
            shared(THIRD, "api:config/app.toml"),
            shared(FIRST, "web:config/app.toml"),
            shared(FIRST, "api:config/z.toml"),
            shared(FIRST, "api:config/app.toml"),
            shared(FIRST, "api:config/app.toml"),
        ] {
            tx.insert_shared_path(A, &entry).await.unwrap();
        }
        tx.insert_shared_path(B, &shared(GADGET, "api:config/app.toml"))
            .await
            .unwrap();
        tx.commit().await.unwrap();

        let mut tx = store.begin().await.unwrap();
        assert_eq!(
            tx.shared_paths(A).await.unwrap(),
            vec![
                shared(FIRST, "api:config/app.toml"),
                shared(FIRST, "api:config/z.toml"),
                shared(FIRST, "web:config/app.toml"),
                shared(THIRD, "api:config/app.toml"),
            ]
        );
        tx.delete_claim(A, FIRST).await.unwrap();
        assert_eq!(tx.shared_paths(A).await.unwrap().len(), 4);
        tx.delete_shared_paths(B, FIRST).await.unwrap();
        assert_eq!(tx.shared_paths(A).await.unwrap().len(), 4);
        tx.delete_shared_paths(A, FIRST).await.unwrap();
        tx.commit().await.unwrap();

        let mut tx = store.begin().await.unwrap();
        assert_eq!(
            tx.shared_paths(A).await.unwrap(),
            vec![shared(THIRD, "api:config/app.toml")]
        );
        assert_eq!(tx.shared_paths(B).await.unwrap().len(), 1);
    }

    async fn grants_are_listed_by_id(store: Arc<dyn Store>) {
        seed(&store).await;
        let grant = |id: u64, item: ItemId, from: MemberId, to: MemberId| GrantRecord {
            id: GrantId(id),
            grant: Grant {
                item,
                area: QualifiedArea::parse("api:billing").unwrap(),
                from,
                to,
            },
            created_at: at(50),
        };
        let mut tx = store.begin().await.unwrap();
        tx.insert_grant(A, &grant(72, THIRD, ALICE, BOB))
            .await
            .unwrap();
        tx.insert_grant(A, &grant(71, FIRST, BOB, ALICE))
            .await
            .unwrap();
        tx.insert_grant(B, &grant(73, GADGET, DAVE, DAVE))
            .await
            .unwrap();
        tx.commit().await.unwrap();

        let mut tx = store.begin().await.unwrap();
        assert_eq!(
            tx.grants(A).await.unwrap(),
            vec![grant(71, FIRST, BOB, ALICE), grant(72, THIRD, ALICE, BOB)]
        );
        assert_eq!(
            tx.grant(A, GrantId(72)).await.unwrap(),
            Some(grant(72, THIRD, ALICE, BOB))
        );
        assert_eq!(tx.grant(B, GrantId(72)).await.unwrap(), None);
        assert!(is_duplicate(
            tx.insert_grant(A, &grant(72, FIRST, BOB, ALICE)).await
        ));
        tx.delete_grant(B, GrantId(71)).await.unwrap();
        tx.delete_grant(A, GrantId(72)).await.unwrap();
        tx.commit().await.unwrap();

        let mut tx = store.begin().await.unwrap();
        let ids: Vec<GrantId> = tx.grants(A).await.unwrap().iter().map(|g| g.id).collect();
        assert_eq!(ids, [GrantId(71)]);
    }

    async fn threads_keep_their_recipients_and_messages_their_order(store: Arc<dyn Store>) {
        seed(&store).await;
        let to_bob = ThreadRecord {
            recipients: Recipients::members(BTreeSet::from([BOB, ALICE])).unwrap(),
            ..thread(ThreadId(81), ALICE, Some(FIRST), 60)
        };
        let to_all = thread(ThreadId(82), BOB, None, 61);
        let mut tx = store.begin().await.unwrap();
        tx.insert_thread(A, &to_bob).await.unwrap();
        tx.insert_thread(A, &to_all).await.unwrap();
        for record in [
            message(93, ThreadId(81), ALICE, 70),
            message(92, ThreadId(81), BOB, 70),
            message(91, ThreadId(81), ALICE, 71),
            message(94, ThreadId(82), BOB, 65),
        ] {
            tx.insert_message(A, &record).await.unwrap();
        }
        tx.commit().await.unwrap();

        let mut tx = store.begin().await.unwrap();
        assert_eq!(tx.thread(A, ThreadId(81)).await.unwrap(), Some(to_bob));
        assert_eq!(tx.thread(A, ThreadId(82)).await.unwrap(), Some(to_all));
        assert_eq!(tx.thread(B, ThreadId(81)).await.unwrap(), None);
        let ids: Vec<MessageId> = tx
            .messages(A, ThreadId(81))
            .await
            .unwrap()
            .iter()
            .map(|m| m.id)
            .collect();
        assert_eq!(ids, [MessageId(92), MessageId(93), MessageId(91)]);
        assert_eq!(
            tx.messages(A, ThreadId(82)).await.unwrap(),
            vec![message(94, ThreadId(82), BOB, 65)]
        );
        assert!(tx.messages(B, ThreadId(81)).await.unwrap().is_empty());

        assert!(is_duplicate(
            tx.insert_thread(A, &thread(ThreadId(81), BOB, None, 62))
                .await
        ));
        assert!(is_duplicate(
            tx.insert_message(A, &message(91, ThreadId(82), BOB, 66))
                .await
        ));
    }

    async fn summaries_count_what_the_reader_has_not_read(store: Arc<dyn Store>) {
        seed(&store).await;
        let t = ThreadId(81);
        let mut tx = store.begin().await.unwrap();
        tx.insert_thread(A, &thread(t, ALICE, None, 60))
            .await
            .unwrap();
        tx.insert_message(A, &message(91, t, ALICE, 61))
            .await
            .unwrap();
        tx.insert_message(A, &message(92, t, BOB, 62))
            .await
            .unwrap();
        tx.insert_message(A, &message(93, t, ALICE, 63))
            .await
            .unwrap();
        tx.commit().await.unwrap();

        let mut tx = store.begin().await.unwrap();
        let counts = |summaries: Vec<dibs_app::records::ThreadSummary>| {
            let s = &summaries[0];
            (s.messages, s.unread, s.last_message_at)
        };
        assert_eq!(
            counts(tx.thread_summaries(A, BOB, None).await.unwrap()),
            (3, 2, Some(at(63)))
        );
        assert_eq!(
            counts(tx.thread_summaries(A, ALICE, None).await.unwrap()),
            (3, 1, Some(at(63)))
        );
        assert!(tx.thread_summaries(B, BOB, None).await.unwrap().is_empty());

        assert_eq!(tx.mark_read(B, BOB, t, at(64)).await.unwrap(), 0);
        assert_eq!(tx.mark_read(A, BOB, t, at(64)).await.unwrap(), 3);
        assert_eq!(tx.mark_read(A, BOB, t, at(65)).await.unwrap(), 0);
        tx.commit().await.unwrap();

        let mut tx = store.begin().await.unwrap();
        assert_eq!(
            counts(tx.thread_summaries(A, BOB, None).await.unwrap()),
            (3, 0, Some(at(63)))
        );
        assert_eq!(
            counts(tx.thread_summaries(A, ALICE, None).await.unwrap()),
            (3, 1, Some(at(63)))
        );
        tx.insert_message(A, &message(94, t, ALICE, 66))
            .await
            .unwrap();
        assert_eq!(
            counts(tx.thread_summaries(A, BOB, None).await.unwrap()),
            (4, 1, Some(at(66)))
        );
        assert_eq!(tx.mark_read(A, BOB, t, at(67)).await.unwrap(), 1);
    }

    async fn summaries_put_the_newest_activity_first_and_filter_by_item(store: Arc<dyn Store>) {
        seed(&store).await;
        let mut tx = store.begin().await.unwrap();
        tx.insert_thread(A, &thread(ThreadId(81), ALICE, Some(FIRST), 60))
            .await
            .unwrap();
        tx.insert_thread(A, &thread(ThreadId(82), ALICE, Some(FIRST), 61))
            .await
            .unwrap();
        tx.insert_thread(A, &thread(ThreadId(83), BOB, None, 75))
            .await
            .unwrap();
        tx.insert_thread(A, &thread(ThreadId(84), BOB, Some(THIRD), 70))
            .await
            .unwrap();
        tx.insert_thread(A, &thread(ThreadId(85), BOB, None, 70))
            .await
            .unwrap();
        tx.insert_message(A, &message(91, ThreadId(81), BOB, 80))
            .await
            .unwrap();
        tx.insert_message(A, &message(92, ThreadId(82), BOB, 62))
            .await
            .unwrap();
        tx.insert_thread(B, &thread(ThreadId(86), DAVE, Some(GADGET), 90))
            .await
            .unwrap();
        tx.commit().await.unwrap();

        let mut tx = store.begin().await.unwrap();
        let order = |summaries: Vec<dibs_app::records::ThreadSummary>| {
            summaries
                .iter()
                .map(|s| s.thread.id.get())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            order(tx.thread_summaries(A, ALICE, None).await.unwrap()),
            [81, 83, 85, 84, 82]
        );
        assert_eq!(
            order(tx.thread_summaries(A, ALICE, Some(FIRST)).await.unwrap()),
            [81, 82]
        );
        assert_eq!(
            order(tx.thread_summaries(B, DAVE, Some(GADGET)).await.unwrap()),
            [86]
        );
        assert!(order(tx.thread_summaries(A, ALICE, Some(GADGET)).await.unwrap()).is_empty());
        assert!(order(tx.thread_summaries(B, DAVE, Some(FIRST)).await.unwrap()).is_empty());
        let quiet = tx
            .thread_summaries(A, ALICE, Some(THIRD))
            .await
            .unwrap()
            .remove(0);
        assert_eq!(
            (quiet.messages, quiet.unread, quiet.last_message_at),
            (0, 0, None)
        );
        assert_eq!(quiet.thread, thread(ThreadId(84), BOB, Some(THIRD), 70));
    }

    async fn events_only_increase_and_page_by_cursor(store: Arc<dyn Store>) {
        seed(&store).await;
        let changed = |by: MemberId| EventKind::SettingsChanged(SettingsChanged { by });
        let mut tx = store.begin().await.unwrap();
        assert_eq!(tx.latest_event(A).await.unwrap(), None);
        assert!(tx.events_after(A, None, 10).await.unwrap().is_empty());
        let mut appended = Vec::new();
        for n in 0..5 {
            appended.push(tx.append_event(A, at(n), &changed(ALICE)).await.unwrap());
            tx.append_event(B, at(n), &changed(DAVE)).await.unwrap();
        }
        tx.commit().await.unwrap();
        let mut tx = store.begin().await.unwrap();
        let later = tx.append_event(A, at(9), &changed(BOB)).await.unwrap();
        tx.commit().await.unwrap();
        appended.push(later);
        assert!(appended.windows(2).all(|pair| pair[0] < pair[1]));

        let mut tx = store.begin().await.unwrap();
        let ids = |events: Vec<Event>| events.iter().map(|e| e.id).collect::<Vec<_>>();
        assert_eq!(ids(tx.events_after(A, None, 100).await.unwrap()), appended);
        assert_eq!(tx.latest_event(A).await.unwrap(), Some(later));
        let first_page = ids(tx.events_after(A, None, 2).await.unwrap());
        assert_eq!(first_page, appended[..2]);
        let second_page = ids(tx.events_after(A, Some(first_page[1]), 2).await.unwrap());
        assert_eq!(second_page, appended[2..4]);
        let last_page = ids(tx.events_after(A, Some(second_page[1]), 2).await.unwrap());
        assert_eq!(last_page, appended[4..]);
        assert!(tx.events_after(A, Some(later), 2).await.unwrap().is_empty());
        assert!(tx.events_after(A, None, 0).await.unwrap().is_empty());

        let theirs = tx.events_after(B, None, 100).await.unwrap();
        assert_eq!(theirs.len(), 5);
        assert!(theirs.iter().all(|e| e.project == B));
        assert!(theirs.iter().all(|e| !appended.contains(&e.id)));
        assert_eq!(
            tx.events_after(ProjectId(9), None, 100).await.unwrap(),
            Vec::new()
        );
    }

    async fn events_keep_every_field(store: Arc<dyn Store>) {
        seed(&store).await;
        let kinds = vec![
            EventKind::ItemCreated(ItemCreated {
                item: FIRST,
                external_ref: Some(ExternalRef::parse("gh:acme/widgets#1").unwrap()),
                title: Title::parse("Fix the invoice totals").unwrap(),
                by: ALICE,
            }),
            EventKind::Claimed(Claimed {
                item: FIRST,
                member: ALICE,
                session: ALICE_LAPTOP,
                host: HostName::parse("laptop").unwrap(),
                areas: areas(&["api:billing", "web:checkout"]),
            }),
            EventKind::Reserved(Reserved {
                item: FIRST,
                member: ALICE,
                areas: areas(&["api:billing"]),
                paths: BTreeSet::from([RepoPath::parse("api:config/app.toml").unwrap()]),
            }),
            EventKind::GrantRevoked(GrantRevoked {
                grant: GrantId(71),
                item: FIRST,
                area: QualifiedArea::parse("api:billing").unwrap(),
                from: ALICE,
                to: BOB,
                cause: GrantEnd::GranteeLeft,
            }),
            EventKind::MessagePosted(MessagePosted {
                thread: ThreadId(81),
                message: MessageId(91),
                author: BOB,
            }),
        ];
        let mut tx = store.begin().await.unwrap();
        let mut expected = Vec::new();
        for (n, kind) in kinds.into_iter().enumerate() {
            let when = at(i64::try_from(n).unwrap());
            let id = tx.append_event(A, when, &kind).await.unwrap();
            expected.push(Event {
                id,
                project: A,
                at: when,
                kind,
            });
        }
        tx.commit().await.unwrap();

        let mut tx = store.begin().await.unwrap();
        assert_eq!(tx.events_after(A, None, 100).await.unwrap(), expected);
        let after: EventId = expected[0].id;
        assert_eq!(
            tx.events_after(A, Some(after), 1).await.unwrap(),
            expected[1..2]
        );
    }

    async fn settings_are_replaced_whole(store: Arc<dyn Store>) {
        seed(&store).await;
        let several: SettingsDocument = serde_json::from_str(
            r#"{
                "stale_after": "48h",
                "edit_policy": "advise",
                "repos": [
                    {"id": "api", "remote": "git@example.com:acme/api.git",
                     "areas": [{"name": "billing", "paths": ["src/billing/**"]}],
                     "hot": [{"paths": ["Cargo.lock"], "kind": "regenerate", "note": "regenerate it"}]},
                    {"id": "web", "remote": "https://example.com/acme/web",
                     "areas": [{"name": "checkout", "paths": ["app/checkout/**"]}]}
                ]
            }"#,
        )
        .unwrap();
        let single: SettingsDocument = serde_json::from_str(
            r#"{"areas": [{"name": "search", "paths": ["packages/search/**"]}]}"#,
        )
        .unwrap();
        let mut tx = store.begin().await.unwrap();
        assert_eq!(tx.settings(A).await.unwrap(), None);
        tx.put_settings(A, &several).await.unwrap();
        tx.put_settings(B, &single).await.unwrap();
        tx.commit().await.unwrap();

        let mut tx = store.begin().await.unwrap();
        assert_eq!(tx.settings(A).await.unwrap(), Some(several));
        tx.put_settings(A, &single).await.unwrap();
        tx.commit().await.unwrap();

        let mut tx = store.begin().await.unwrap();
        assert_eq!(tx.settings(A).await.unwrap(), Some(single.clone()));
        assert_eq!(tx.settings(B).await.unwrap(), Some(single));
    }

    async fn browser_sessions_are_found_by_hash_and_swept_when_ended(store: Arc<dyn Store>) {
        seed(&store).await;
        let browser = |hash: &str, member: MemberId, token: &str, ends: i64| UiSessionRecord {
            hash: TokenHash::new(hash.to_owned()),
            member,
            token: TokenHash::new(token.to_owned()),
            expires_at: at(ends),
        };
        let mut tx = store.begin().await.unwrap();
        tx.insert_token(A, &token("hash:alice", "AAAAAAAA", ALICE, 1))
            .await
            .unwrap();
        tx.insert_token(B, &token("hash:dave", "DDDDDDDD", DAVE, 1))
            .await
            .unwrap();
        tx.insert_ui_session(A, &browser("ui:ended", ALICE, "hash:alice", 100))
            .await
            .unwrap();
        tx.insert_ui_session(A, &browser("ui:now", ALICE, "hash:alice", 200))
            .await
            .unwrap();
        tx.insert_ui_session(B, &browser("ui:dave", DAVE, "hash:dave", 150))
            .await
            .unwrap();
        tx.commit().await.unwrap();

        let mut tx = store.begin().await.unwrap();
        let hash = |text: &str| TokenHash::new(text.to_owned());
        assert_eq!(
            tx.ui_session(&hash("ui:dave")).await.unwrap(),
            Some(InProject {
                project: B,
                record: browser("ui:dave", DAVE, "hash:dave", 150),
            })
        );
        assert_eq!(tx.ui_session(&hash("ui:none")).await.unwrap(), None);
        assert!(is_duplicate(
            tx.insert_ui_session(A, &browser("ui:dave", ALICE, "hash:alice", 300))
                .await
        ));

        tx.extend_ui_session(B, &hash("ui:now"), at(900))
            .await
            .unwrap();
        tx.delete_ui_session(B, &hash("ui:ended")).await.unwrap();
        assert_eq!(
            tx.ui_session(&hash("ui:now"))
                .await
                .unwrap()
                .map(|s| s.record.expires_at),
            Some(at(200))
        );
        assert!(tx.ui_session(&hash("ui:ended")).await.unwrap().is_some());
        tx.extend_ui_session(B, &hash("ui:dave"), at(250))
            .await
            .unwrap();
        tx.commit().await.unwrap();

        let mut tx = store.begin().await.unwrap();
        assert_eq!(tx.delete_expired_ui_sessions(at(200)).await.unwrap(), 1);
        assert!(tx.ui_session(&hash("ui:ended")).await.unwrap().is_none());
        assert!(tx.ui_session(&hash("ui:now")).await.unwrap().is_some());
        assert!(tx.ui_session(&hash("ui:dave")).await.unwrap().is_some());
        tx.delete_ui_session(A, &hash("ui:now")).await.unwrap();
        assert!(tx.ui_session(&hash("ui:now")).await.unwrap().is_none());
        assert_eq!(tx.delete_expired_ui_sessions(at(251)).await.unwrap(), 1);
        assert_eq!(tx.delete_expired_ui_sessions(at(999)).await.unwrap(), 0);
    }

    async fn a_reference_to_a_missing_record_fails_the_commit_and_writes_nothing(
        store: Arc<dyn Store>,
    ) {
        seed(&store).await;
        let mut tx = store.begin().await.unwrap();
        tx.insert_token(B, &token("hash:dave", "DDDDDDDD", DAVE, 1))
            .await
            .unwrap();
        tx.commit().await.unwrap();

        let fresh = project(ProjectId(3), "fresh");
        let refused = |what: &'static str| {
            move |result: Result<(), StoreError>| {
                assert!(matches!(result, Err(StoreError::Backend(_))), "{what}");
            }
        };

        let mut tx = store.begin().await.unwrap();
        tx.insert_project(&fresh).await.unwrap();
        tx.save_claim(A, &claim(ItemId(99), ALICE, ALICE_LAPTOP, &[]))
            .await
            .unwrap();
        refused("a claim on a missing item")(tx.commit().await);

        let mut tx = store.begin().await.unwrap();
        tx.insert_project(&fresh).await.unwrap();
        tx.save_claim(A, &claim(GADGET, ALICE, ALICE_LAPTOP, &[]))
            .await
            .unwrap();
        refused("a claim on another project's item")(tx.commit().await);

        let mut tx = store.begin().await.unwrap();
        tx.insert_project(&fresh).await.unwrap();
        tx.insert_message(A, &message(91, ThreadId(99), ALICE, 60))
            .await
            .unwrap();
        refused("a message in a missing thread")(tx.commit().await);

        let mut tx = store.begin().await.unwrap();
        tx.insert_project(&fresh).await.unwrap();
        tx.insert_member(ProjectId(9), &member(MemberId(19), "erin"))
            .await
            .unwrap();
        refused("a member of a missing project")(tx.commit().await);

        let mut tx = store.begin().await.unwrap();
        tx.insert_project(&fresh).await.unwrap();
        let grant = GrantRecord {
            id: GrantId(71),
            grant: Grant {
                item: FIRST,
                area: QualifiedArea::parse("api:billing").unwrap(),
                from: ALICE,
                to: DAVE,
            },
            created_at: at(50),
        };
        tx.insert_grant(A, &grant).await.unwrap();
        refused("a grant to a member of another project")(tx.commit().await);

        let mut tx = store.begin().await.unwrap();
        tx.insert_project(&fresh).await.unwrap();
        let browser = UiSessionRecord {
            hash: TokenHash::new("ui:borrowed".to_owned()),
            member: ALICE,
            token: TokenHash::new("hash:dave".to_owned()),
            expires_at: at(100),
        };
        tx.insert_ui_session(A, &browser).await.unwrap();
        refused("a browser session opened by another project's token")(tx.commit().await);

        let mut tx = store.begin().await.unwrap();
        assert_eq!(tx.project(ProjectId(3)).await.unwrap(), None);
        assert_eq!(tx.claim(A, ItemId(99)).await.unwrap(), None);
        assert_eq!(tx.claim(A, GADGET).await.unwrap(), None);
        assert!(tx.messages(A, ThreadId(99)).await.unwrap().is_empty());
        assert!(tx.members(ProjectId(9)).await.unwrap().is_empty());
        assert!(tx.grants(A).await.unwrap().is_empty());
        assert_eq!(
            tx.ui_session(&TokenHash::new("ui:borrowed".to_owned()))
                .await
                .unwrap(),
            None
        );
        tx.insert_project(&fresh).await.unwrap();
        tx.commit().await.unwrap();
    }

    async fn the_largest_id_the_port_allows_is_kept(store: Arc<dyn Store>) {
        let largest = u64::try_from(i64::MAX).unwrap();
        let mut tx = store.begin().await.unwrap();
        tx.insert_project(&project(ProjectId(largest), "widgets"))
            .await
            .unwrap();
        tx.insert_member(ProjectId(largest), &member(MemberId(largest), "alice"))
            .await
            .unwrap();
        tx.commit().await.unwrap();

        let mut tx = store.begin().await.unwrap();
        assert_eq!(
            tx.member(ProjectId(largest), MemberId(largest))
                .await
                .unwrap(),
            Some(member(MemberId(largest), "alice"))
        );
        tx.append_event(
            ProjectId(largest),
            at(1),
            &EventKind::SettingsChanged(SettingsChanged {
                by: MemberId(largest),
            }),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }
}

//! Races through the public API on a multi-threaded runtime: many sessions claiming one item, members
//! claiming one area through different items, and several takeovers of one stale claim. The store
//! runs one transaction at a time, so each race has exactly one winner and every loser is told who
//! won.

#[cfg(test)]
mod concurrency {
    use std::sync::Arc;

    use time::{Duration, OffsetDateTime};

    use dibs_app::fakes::Fakes;
    use dibs_app::named::Refusal;
    use dibs_app::use_cases::{
        ClaimItemInput, ClaimMade, CreateItemInput, CreateMemberInput, InitProjectInput, NewMember,
        RegisterSessionInput, TakeOverInput, UseCases,
    };
    use dibs_app::{Actor, AppError};
    use dibs_domain::claim::{ClaimOutcome, ClaimRefused, TakeoverRefused};
    use dibs_domain::handle::Handle;
    use dibs_domain::ids::{ItemId, MemberId, SessionId};
    use dibs_domain::item::{ExternalRef, Title};
    use dibs_domain::member::DisplayName;
    use dibs_domain::member::Role;
    use dibs_domain::message::{Body, SessionLabel};
    use dibs_domain::project::ProjectSlug;
    use dibs_domain::session::AgentLabel;
    use dibs_domain::session::HostName;
    use dibs_domain::settings::SettingsDocument;

    const RACERS: usize = 32;

    struct Racer {
        actor: Actor,
        session: SessionId,
    }

    struct Race {
        fakes: Fakes,
        app: UseCases,
        racers: Vec<Racer>,
    }

    impl Race {
        async fn new() -> Arc<Self> {
            let fakes = Fakes::new(OffsetDateTime::UNIX_EPOCH);
            let app = UseCases::new(&fakes.dependencies());
            let member = |handle: &str| NewMember {
                handle: Handle::parse(handle).unwrap(),
                display_name: DisplayName::parse(handle).unwrap(),
                external_login: None,
            };
            let created = app
                .init_project
                .execute(InitProjectInput {
                    slug: ProjectSlug::parse("widgets").unwrap(),
                    admin: member("admin"),
                })
                .await
                .unwrap();
            let admin = app
                .authenticate
                .execute(created.token.expose())
                .await
                .unwrap();
            let document: SettingsDocument =
                serde_json::from_str(r#"{"areas":[{"name":"billing","paths":["billing/**"]}]}"#)
                    .unwrap();
            app.apply_settings.execute(&admin, document).await.unwrap();
            let mut racers = Vec::new();
            for n in 0..RACERS {
                let token = app
                    .create_member
                    .execute(
                        &admin,
                        CreateMemberInput {
                            member: member(&format!("racer-{n}")),
                            role: Role::Member,
                        },
                    )
                    .await
                    .unwrap()
                    .token;
                let actor = app.authenticate.execute(token.expose()).await.unwrap();
                let session = app
                    .register_session
                    .execute(
                        &actor,
                        RegisterSessionInput {
                            host: HostName::parse(&format!("host-{n}")).unwrap(),
                            label: SessionLabel::parse(&format!("host-{n}/mcp")).unwrap(),
                            agent: AgentLabel::parse("agent").unwrap(),
                            push: false,
                        },
                    )
                    .await
                    .unwrap()
                    .id;
                racers.push(Racer { actor, session });
            }
            Arc::new(Self { fakes, app, racers })
        }

        async fn item(&self, n: usize) -> ItemId {
            self.app
                .create_item
                .execute(
                    &self.racers[0].actor,
                    CreateItemInput {
                        external_ref: Some(
                            ExternalRef::parse(&format!("gh:acme/widgets#{n}")).unwrap(),
                        ),
                        title: Title::parse("Contested").unwrap(),
                    },
                )
                .await
                .unwrap()
                .item
                .item
                .id
        }

        /// Every racer claims at once: racer `n` claims `items(n)` declaring `areas`.
        async fn claim_all(
            self: &Arc<Self>,
            items: impl Fn(usize) -> ItemId,
            areas: &[&str],
        ) -> Vec<(MemberId, Result<ClaimMade, AppError>)> {
            let mut tasks = Vec::new();
            for n in 0..RACERS {
                let race = self.clone();
                let input = ClaimItemInput {
                    wide: None,
                    item: items(n),
                    session: race.racers[n].session,
                    areas: areas.iter().map(|a| (*a).to_owned()).collect(),
                };
                tasks.push(tokio::spawn(async move {
                    let racer = &race.racers[n];
                    (
                        racer.actor.member,
                        race.app.claim_item.execute(&racer.actor, input).await,
                    )
                }));
            }
            let mut results = Vec::new();
            for task in tasks {
                results.push(task.await.unwrap());
            }
            results
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn one_item_has_one_winner_and_every_loser_learns_who() {
        let race = Race::new().await;
        let item = race.item(1).await;
        let results = race.claim_all(|_| item, &[]).await;
        let winners: Vec<MemberId> = results
            .iter()
            .filter(|(_, r)| {
                matches!(
                    r,
                    Ok(ClaimMade {
                        outcome: ClaimOutcome::New { .. },
                        ..
                    })
                )
            })
            .map(|(member, _)| *member)
            .collect();
        assert_eq!(winners.len(), 1, "{results:?}");
        let winner = winners[0];
        for (member, result) in &results {
            if *member == winner {
                continue;
            }
            assert!(
                matches!(
                    result,
                    Err(AppError::Claim(Refusal {
                        reason: ClaimRefused::HeldBy { member: holder, .. },
                        member: Some(_),
                        ..
                    })) if *holder == winner
                ),
                "{result:?}"
            );
        }
        let view = race
            .app
            .get_item
            .execute(&race.racers[0].actor, item)
            .await
            .unwrap();
        assert_eq!(view.claim.map(|c| c.claim.member), Some(winner));
        let claimed = race
            .fakes
            .store
            .all_events()
            .await
            .iter()
            .filter(|e| e.kind.name() == "claimed")
            .count();
        assert_eq!(claimed, 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn one_area_has_one_holder_across_items() {
        let race = Race::new().await;
        let mut items = Vec::new();
        for n in 0..RACERS {
            items.push(race.item(n).await);
        }
        let results = race.claim_all(|n| items[n], &["billing"]).await;
        let winners: Vec<MemberId> = results
            .iter()
            .filter(|(_, r)| r.is_ok())
            .map(|(member, _)| *member)
            .collect();
        assert_eq!(winners.len(), 1, "{results:?}");
        for (member, result) in &results {
            if *member != winners[0] {
                assert!(
                    matches!(
                        result,
                        Err(AppError::Claim(Refusal {
                            reason: ClaimRefused::AreaHeld { member: holder, .. },
                            member: Some(_),
                            ..
                        })) if *holder == winners[0]
                    ),
                    "{result:?}"
                );
            }
        }
        let status = race
            .app
            .status
            .execute(&race.racers[0].actor)
            .await
            .unwrap();
        assert_eq!(status.holds.len(), 1);
        assert_eq!(status.holds[0].member, winners[0]);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn a_stale_claim_is_taken_over_once() {
        let race = Race::new().await;
        let item = race.item(1).await;
        race.app
            .claim_item
            .execute(
                &race.racers[0].actor,
                ClaimItemInput {
                    wide: None,
                    item,
                    session: race.racers[0].session,
                    areas: Vec::new(),
                },
            )
            .await
            .unwrap();
        race.fakes.clock.advance(Duration::hours(25));
        let mut tasks = Vec::new();
        for n in 1..RACERS {
            let race = race.clone();
            tasks.push(tokio::spawn(async move {
                let racer = &race.racers[n];
                race.app
                    .take_over
                    .execute(
                        &racer.actor,
                        TakeOverInput {
                            item,
                            session: racer.session,
                            areas: Vec::new(),
                            reason: Body::parse("Picking this up.").unwrap(),
                        },
                    )
                    .await
            }));
        }
        let mut won = 0;
        for task in tasks {
            match task.await.unwrap() {
                Ok(_) => won += 1,
                Err(error) => assert!(
                    matches!(
                        error,
                        AppError::Takeover(Refusal {
                            reason: TakeoverRefused::NotStale { .. },
                            ..
                        })
                    ),
                    "{error:?}"
                ),
            }
        }
        assert_eq!(won, 1);
        let taken = race
            .fakes
            .store
            .all_events()
            .await
            .iter()
            .filter(|e| e.kind.name() == "taken_over")
            .count();
        assert_eq!(taken, 1);
    }
}

//! Managing a project: members, tokens, settings and forced releases.

use dibs_client::api::{ClaimRequest, NewItem, NewMember};
use dibs_domain::member::Role;
use dibs_domain::settings::SettingsDocument;

use crate::support::{World, client, run, session};

#[test]
fn an_admin_adds_a_member_whose_token_signs_in_until_rotated() {
    let world = World::new();
    let alice = world.client(&world.alice);
    run(async {
        let enrolled = alice
            .add_member(&NewMember {
                handle: "carol".parse().unwrap(),
                display_name: "Carol".parse().unwrap(),
                external_login: Some("carol-gh".parse().unwrap()),
                role: Role::Member,
            })
            .await
            .unwrap();
        assert_eq!(enrolled.member.handle.as_str(), "carol");
        let carol = client(&world.server.url(), enrolled.token.expose());
        assert_eq!(carol.me().await.unwrap().member.handle.as_str(), "carol");

        let taken = alice
            .add_member(&NewMember {
                handle: "carol".parse().unwrap(),
                display_name: "Carol".parse().unwrap(),
                external_login: None,
                role: Role::Member,
            })
            .await
            .unwrap_err();
        assert_eq!(taken.code(), "handle_taken");

        let rotated = alice.rotate_token(&"carol".parse().unwrap()).await.unwrap();
        assert_ne!(rotated.token, enrolled.token);
        assert_eq!(carol.me().await.unwrap_err().code(), "unauthorized");
        let carol = client(&world.server.url(), rotated.token.expose());
        assert!(carol.me().await.is_ok());

        let prefix: String = rotated.token.expose()["dibs_".len()..]
            .chars()
            .take(8)
            .collect();
        let revoked = alice.revoke_token(&prefix).await.unwrap();
        assert!(revoked.token.revoked_at.is_some());
        assert_eq!(carol.me().await.unwrap_err().code(), "unauthorized");

        let bob = world.client(&world.bob);
        let forbidden = bob
            .rotate_token(&"alice".parse().unwrap())
            .await
            .unwrap_err();
        assert_eq!(forbidden.code(), "forbidden");
    });
}

#[test]
fn settings_are_applied_and_answered_in_normal_form() {
    let world = World::new();
    let alice = world.client(&world.alice);
    run(async {
        let document: SettingsDocument = serde_json::from_value(serde_json::json!({
            "stale_after": "2h",
            "heartbeat_every": "300s",
            "areas": [{"name": "billing", "paths": ["billing/**"]}]
        }))
        .unwrap();
        let applied = alice.apply_settings(&document).await.unwrap();
        assert!(applied.changed);
        assert_eq!(applied.settings.areas.len(), 1);
        assert_eq!(applied.settings.heartbeat_every.as_deref(), Some("5m"));
        let me = alice.me().await.unwrap();
        assert_eq!(
            me.project_settings().unwrap().heartbeat_every(),
            time::Duration::minutes(5)
        );
        let too_slow = SettingsDocument {
            heartbeat_every: Some("16m".to_owned()),
            ..SettingsDocument::default()
        };
        assert_eq!(
            alice.apply_settings(&too_slow).await.unwrap_err().code(),
            "invalid_settings"
        );
        assert!(!alice.apply_settings(&document).await.unwrap().changed);
        let invalid = SettingsDocument {
            stale_after: Some("1s".to_owned()),
            ..SettingsDocument::default()
        };
        assert_eq!(
            alice.apply_settings(&invalid).await.unwrap_err().code(),
            "invalid_settings"
        );
    });
}

#[test]
fn an_admin_force_releases_any_claim() {
    let world = World::new();
    let alice = world.client(&world.alice);
    let bob = world.client(&world.bob);
    run(async {
        let item = bob
            .create_item(&NewItem {
                external_ref: None,
                title: "Local work".parse().unwrap(),
            })
            .await
            .unwrap()
            .item
            .id;
        let bob_session = session(&bob, "desktop/cli").await.session_id;
        bob.claim(
            item,
            &ClaimRequest {
                wide: None,
                session_id: bob_session,
                areas: Vec::new(),
            },
        )
        .await
        .unwrap();
        assert_eq!(
            bob.force_release(item).await.unwrap_err().code(),
            "forbidden"
        );
        let ended = alice.force_release(item).await.unwrap();
        assert_eq!(ended.claim.holder.as_str(), "bob");
    });
}

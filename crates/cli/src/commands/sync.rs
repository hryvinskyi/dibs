//! `dibs sync`: what a hook leaves to the background, done now — the heartbeat when it is due,
//! the settings, status and inbox caches refreshed when they are old, and, with a fresh inbox, the
//! deliveries it owes claimed for the session of a host that can show them.
//!
//! With `--update`, and at most once a day, it also asks the server which client it recommends and,
//! when that is newer than this one, installs it as `dibs self-update` would — after the sync lock
//! is let go, so the download keeps no hook's sync waiting — where the platform lets the background
//! install ([`release::BACKGROUND_UPDATE`]); elsewhere it only remembers the version for the brief. The check is made even while the server
//! counts as unreachable, as that is how a client the server no longer serves learns what to
//! update to.
//!
//! One runs at a time per project (it holds the project's sync lock for its whole run), every
//! call is bounded to two seconds, and while the server counted as unreachable within the last
//! minute nothing is tried. Each answer is written to its cache in one step, so a hook reading
//! it never sees half of one.

use std::time::Duration as StdDuration;

use serde::Serialize;
use time::OffsetDateTime;

use dibs_agent_host::delivery;
use dibs_agent_host::ports::{ClaimedDelivery as GuardClaim, LocalCache, Pending, SessionRef};
use dibs_agent_host::upkeep::{DEFAULT_HEARTBEAT, Due, Freshness, circuit_open};
use dibs_agent_host::{HostId, SessionKey};
use dibs_client::Client;
use dibs_client::checkout::Checkout;
use dibs_client::update::{Updated, Wanted};
use semver::Version;

use crate::cli::SyncArgs;
use crate::commands::self_update;
use crate::context::{AGENT, Context};
use crate::failure::{Failure, Kind};
use crate::guard::cache::StateCache;
use crate::guard::remote::{Remote, RemoteError};
use crate::guard::upkeep::take_lock;
use crate::guard::views;
use crate::hosts;
use crate::release::{self, BackgroundUpdate};

/// How long a sync waits for the hook that started it to let go of the lock.
const LOCK_PATIENCE: StdDuration = StdDuration::from_secs(1);

/// What a sync did.
#[derive(Debug, Default, Serialize)]
struct Synced {
    settings: bool,
    heartbeat: bool,
    status: bool,
    inbox: bool,
    /// How many deliveries it claimed for the session, and how many of them it won.
    claimed: usize,
    won: usize,
    /// What the check for a newer client came to, when one was made.
    #[serde(skip_serializing_if = "Option::is_none")]
    update: Option<String>,
}

/// Runs `dibs sync`.
///
/// # Errors
/// A configuration [`Failure`] outside a checkout or without a token; an unreachable one when the
/// server could not be reached, now or within the last minute.
pub async fn sync(args: SyncArgs, ctx: &Context) -> Result<(), Failure> {
    let Some(checkout) = ctx.checkout()? else {
        return Err(Failure::config(
            "no_checkout",
            "No .dibs.toml was found here or above in this repository; run dibs sync inside a checkout.",
        ));
    };
    let remote = Remote::new(&checkout, &ctx.locations()?, ctx.env());
    let lock = take_lock(&remote.state, LOCK_PATIENCE).map_err(|error| {
        tracing::debug!(error = %error, "the sync lock could not be taken");
        Failure::config(
            "state",
            "The project's sync lock could not be taken on this machine.",
        )
    })?;
    let Some(held) = lock else {
        say(
            &args,
            ctx,
            "Another dibs sync is running for this project; it does the work.",
        );
        return Ok(());
    };
    let now = OffsetDateTime::now_utc();
    let cache = StateCache::new(remote.state.clone(), checkout.root());
    let update_due = args.update && (args.force || Due::at(now, &freshness(&cache)).update);
    if circuit_open(cache.unreachable_since(), now) {
        let recommended = if update_due {
            recommended(&remote, now).await
        } else {
            None
        };
        drop(held);
        if let Some(latest) = recommended {
            let update = Some(install_recommended(latest, release::BACKGROUND_UPDATE, ctx).await);
            report(
                &args,
                ctx,
                &Synced {
                    update,
                    ..Synced::default()
                },
            );
        }
        return Err(unreachable(
            "The server could not be reached less than a minute ago; nothing was synced.",
        ));
    }
    let due = if args.force {
        Due {
            settings: true,
            heartbeat: true,
            status: true,
            inbox: true,
            update: false,
        }
    } else {
        Due::at(now, &freshness(&cache))
    };
    let session = session(&args);
    let client = remote.client().map_err(config)?;
    let result = run(&remote, &client, &checkout, session.as_ref(), due, now).await;
    let recommended = if update_due {
        recommended(&remote, now).await
    } else {
        None
    };
    drop(held);
    let update = match recommended {
        Some(latest) => Some(install_recommended(latest, release::BACKGROUND_UPDATE, ctx).await),
        None => None,
    };
    match result {
        Ok(mut synced) => {
            if let Err(error) = remote.state.clear_unreachable() {
                tracing::debug!(error = %error, "the unreachable mark could not be cleared");
            }
            synced.update = update;
            report(&args, ctx, &synced);
            Ok(())
        }
        Err(RemoteError::Client(error)) if error.is_unavailable() => {
            cache.mark_unreachable(now);
            Err(Failure::from(error))
        }
        Err(RemoteError::Client(error)) => Err(Failure::from(error)),
        Err(RemoteError::Config(text)) => Err(Failure::config("sync", text)),
    }
}

/// Asks the server which client it recommends and remembers the answer, which starts the day
/// until the next check; answers the recommended version when it is newer than this one.
async fn recommended(remote: &Remote, now: OffsetDateTime) -> Option<Version> {
    let client = match remote.client() {
        Ok(client) => client,
        Err(error) => {
            tracing::debug!(error = ?error, "no client to ask for the recommended version");
            return None;
        }
    };
    let versions = match client.versions().await {
        Ok(versions) => versions,
        Err(error) => {
            tracing::debug!(
                code = error.code(),
                "the recommended version could not be asked for"
            );
            return None;
        }
    };
    if let Err(error) = remote.state.remember_versions(&versions, now) {
        tracing::debug!(error = %error, "the client versions could not be remembered");
    }
    let installed = release::installed().ok()?;
    versions.latest_client.filter(|latest| *latest > installed)
}

/// Installs the recommended version as `dibs self-update --version` would, where the platform lets
/// the background install, and says what came of it.
async fn install_recommended(latest: Version, policy: BackgroundUpdate, ctx: &Context) -> String {
    if policy == BackgroundUpdate::NotifyOnly {
        return notify_only(&latest);
    }
    match self_update::install(Wanted::Exactly(latest.clone()), ctx).await {
        Ok(Updated::Installed { from, to }) => format!("Updated dibs from {from} to {to}."),
        Ok(Updated::Current(version)) => format!("dibs {version} is installed."),
        Err(failure) => {
            tracing::debug!(
                code = failure.code,
                "the recommended version was not installed"
            );
            format!("dibs {latest} was not installed: {}", failure.message)
        }
    }
}

/// What the background says of a recommended version it does not install.
fn notify_only(latest: &Version) -> String {
    format!("dibs {latest} is recommended; run dibs self-update in a terminal to install it.")
}

async fn run(
    remote: &Remote,
    client: &Client,
    checkout: &Checkout,
    session: Option<&SessionRef>,
    due: Due,
    now: OffsetDateTime,
) -> Result<Synced, RemoteError> {
    let mut synced = Synced::default();
    if due.settings {
        remote.me(client, checkout, now).await?;
        synced.settings = true;
    }
    if let Some(session) = session.filter(|_| due.heartbeat) {
        let id = remote.session(client, session).await?;
        client.heartbeat(id).await?;
        if let Err(error) = remote.state.remember_heartbeat(now) {
            tracing::debug!(error = %error, "the heartbeat could not be remembered");
        }
        synced.heartbeat = true;
    }
    if due.status {
        let status = client.status().await?;
        if let Err(error) = remote.state.remember_status(&status, now) {
            tracing::debug!(error = %error, "the status could not be cached");
        }
        synced.status = true;
    }
    if due.inbox {
        let inbox = client.inbox(views::GUARD_INBOX).await?;
        if let Err(error) = remote.state.remember_inbox(&inbox, now) {
            tracing::debug!(error = %error, "the inbox could not be cached");
        }
        synced.inbox = true;
        if let Some(session) = session.filter(|session| delivers_to(session)) {
            claim_deliveries(remote, client, session, now, &mut synced).await?;
        }
    }
    Ok(synced)
}

/// Whether the session's host can be shown what is delivered between its tool calls.
fn delivers_to(session: &SessionRef) -> bool {
    hosts::registry()
        .get(session.host.as_str())
        .is_some_and(|adapter| delivery::can_deliver(adapter.capabilities()))
}

/// Claims, for `session`, the deliveries the freshly cached inbox owes the member that this machine
/// has not claimed yet, and remembers each one won or lost beside the inbox, so the session's next
/// hook on its main thread shows what it won. A server that stops answering ends the claims; what
/// was claimed until then is remembered.
async fn claim_deliveries(
    remote: &Remote,
    client: &Client,
    session: &SessionRef,
    now: OffsetDateTime,
    synced: &mut Synced,
) -> Result<(), RemoteError> {
    let (Some(cached), Some(member)) = (remote.state.inbox(), remote.state.member()) else {
        return Ok(());
    };
    let view = views::inbox(&cached.inbox, &cached.claims);
    let picks = delivery::to_claim(&view, &member);
    if picks.is_empty() {
        return Ok(());
    }
    let mut claims = delivery::kept_claims(view.claims, now);
    let id = remote.session(client, session).await?;
    let mut failure = None;
    for pending in picks {
        let answer = match &pending {
            Pending::Message(message) => client.deliver_message(message.message, id).await,
            Pending::Offer(offer) => client.deliver_offer(&offer.slot, offer.until, id).await,
        };
        let won = match answer {
            Ok(_) => true,
            Err(error) if error.is_unavailable() => {
                failure = Some(error);
                break;
            }
            Err(error) => {
                tracing::debug!(code = error.code(), "a delivery went to another session");
                false
            }
        };
        synced.claimed += 1;
        synced.won += usize::from(won);
        claims.push(GuardClaim {
            pending,
            host: session.host.as_str().to_owned(),
            session: session.key.clone(),
            won,
            at: now,
        });
    }
    let remembered: Vec<_> = claims.iter().map(views::remembered).collect();
    if let Err(error) = remote.state.remember_claims(&remembered) {
        tracing::debug!(error = %error, "the claimed deliveries could not be remembered");
    }
    failure.map_or(Ok(()), |error| Err(RemoteError::Client(error)))
}

/// When each cache was filled, the heartbeat interval from the cached settings.
fn freshness(cache: &StateCache) -> Freshness {
    let settings = cache.settings().ok().flatten();
    Freshness {
        heartbeat_every: settings.as_ref().map_or(DEFAULT_HEARTBEAT, |cached| {
            cached.value.settings.heartbeat_every()
        }),
        last_heartbeat: cache.last_heartbeat(),
        settings_at: settings.as_ref().map(|cached| cached.at),
        status_at: cache.status().map(|cached| cached.at),
        inbox_at: cache.inbox().map(|cached| cached.at),
        versions_at: cache.versions_checked_at(),
    }
}

/// The session the heartbeat is for: the host's session the command names, or this machine's
/// command-line session when it names none. A host this dibs does not link, or a session id no
/// host could have sent, has no session to keep alive.
fn session(args: &SyncArgs) -> Option<SessionRef> {
    let (Some(host), Some(key)) = (args.host.as_deref(), args.session.as_deref()) else {
        return SessionKey::parse(AGENT).map(|key| SessionRef {
            host: HostId::new(AGENT),
            key,
        });
    };
    let host = hosts::registry().get(host)?.id();
    SessionKey::parse(key).map(|key| SessionRef { host, key })
}

fn config(error: RemoteError) -> Failure {
    match error {
        RemoteError::Config(text) => Failure::config("credentials", text),
        RemoteError::Client(error) => Failure::from(error),
    }
}

fn unreachable(message: &str) -> Failure {
    Failure {
        kind: Kind::Unreachable,
        code: "unreachable".to_owned(),
        message: message.to_owned(),
        details: None,
        answered: false,
    }
}

fn say(args: &SyncArgs, ctx: &Context, text: &str) {
    if !args.quiet {
        ctx.printer.show(
            &serde_json::json!({"synced": false, "message": text}),
            || text.to_owned(),
        );
    }
}

fn report(args: &SyncArgs, ctx: &Context, synced: &Synced) {
    if args.quiet {
        return;
    }
    let done: Vec<&str> = [
        (synced.settings, "settings"),
        (synced.heartbeat, "heartbeat"),
        (synced.status, "status"),
        (synced.inbox, "inbox"),
    ]
    .iter()
    .filter(|(did, _)| *did)
    .map(|(_, what)| *what)
    .collect();
    ctx.printer.show(synced, || {
        let mut text = if done.is_empty() {
            "Nothing was due.".to_owned()
        } else {
            format!("Synced: {}.", done.join(", "))
        };
        if synced.claimed > 0 {
            text.push_str(&format!(
                " Claimed {} deliveries for the session, {} of them its own to show.",
                synced.claimed, synced.won
            ));
        }
        if let Some(update) = &synced.update {
            text.push(' ');
            text.push_str(update);
        }
        text
    });
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::output::Printer;

    #[test]
    fn a_background_that_only_tells_installs_nothing_and_says_how_to_update() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = Context::new(
            HashMap::new(),
            dir.path().to_path_buf(),
            Printer::new(false),
        );
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let said = runtime.block_on(install_recommended(
            Version::new(9, 0, 0),
            BackgroundUpdate::NotifyOnly,
            &ctx,
        ));
        assert_eq!(said, notify_only(&Version::new(9, 0, 0)));
        assert!(said.contains("run dibs self-update"), "{said}");
        assert!(std::fs::read_dir(dir.path()).unwrap().next().is_none());
    }
}

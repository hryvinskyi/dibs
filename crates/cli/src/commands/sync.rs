//! `dibs sync`: what a hook leaves to the background, done now — the heartbeat when it is due,
//! the settings, status and inbox caches refreshed when they are old, and, with a fresh inbox, the
//! deliveries it owes claimed for the session of a host that can show them.
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

use crate::cli::SyncArgs;
use crate::context::{AGENT, Context};
use crate::failure::{Failure, Kind};
use crate::guard::cache::StateCache;
use crate::guard::remote::{Remote, RemoteError};
use crate::guard::upkeep::take_lock;
use crate::guard::views;
use crate::hosts;

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
    let Some(_lock) = lock else {
        say(
            &args,
            ctx,
            "Another dibs sync is running for this project; it does the work.",
        );
        return Ok(());
    };
    let now = OffsetDateTime::now_utc();
    let cache = StateCache::new(remote.state.clone(), checkout.root());
    if circuit_open(cache.unreachable_since(), now) {
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
        }
    } else {
        Due::at(now, &freshness(&cache))
    };
    let session = session(&args);
    let client = remote.client().map_err(config)?;
    match run(&remote, &client, &checkout, session.as_ref(), due, now).await {
        Ok(synced) => {
            if let Err(error) = remote.state.clear_unreachable() {
                tracing::debug!(error = %error, "the unreachable mark could not be cleared");
            }
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
        if done.is_empty() {
            return "Nothing was due.".to_owned();
        }
        let mut text = format!("Synced: {}.", done.join(", "));
        if synced.claimed > 0 {
            text.push_str(&format!(
                " Claimed {} deliveries for the session, {} of them its own to show.",
                synced.claimed, synced.won
            ));
        }
        text
    });
}

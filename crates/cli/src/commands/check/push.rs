//! `dibs check --push`: the push git describes to its pre-push hook, judged by the rule the agents'
//! command check uses. It knows the exact refs pushed to whatever the command line said and
//! whoever typed it, so it covers a push from a terminal and one with no refspec.
//!
//! It refuses only on a verdict: a protected branch or tag of the repository pushed to that the
//! member is not among the pushers of — judged from what this machine remembers, so it holds while the server
//! cannot be reached — or a ref frozen by a slot another member holds. Anything else that keeps it
//! from deciding — no checkout, no token, a server that cannot be reached for a freeze, a machine
//! that remembers nothing yet and cannot reach the server — lets the push go with one warning;
//! `git push --no-verify` stays the conscious way past a refusal, and for a protected branch it
//! breaks the team's rule.
//!
//! A refusal is counted, by its kind, in what this machine reports to the project, and named by the
//! id it is counted under. With `DIBS_OFF` set the push is let go without a word and without any
//! call.

use dibs_agent_host::ports::{Workspace, Workspaces};
use dibs_agent_host::slots::KnownBoard;
use dibs_agent_host::wording;
use dibs_domain::handle::Handle;
use dibs_domain::push::PushProtection;
use dibs_domain::repo::RepoId;
use dibs_domain::report::{ReportDay, Tally};
use dibs_domain::settings::ProjectSettings;
use dibs_domain::slot::{CommandVerdict, RepoRef, SlotName, push_verdict};
use time::OffsetDateTime;

use crate::commands::check::Governed;
use crate::context::{Connection, Context};
use crate::failure::Failure;
use crate::guard::views;
use crate::guard::workspace::ClientWorkspaces;
use crate::guard::{OFF_VAR, switched_off};

/// The remote refs of the lines git writes to a pre-push hook: `<local ref> <local sha> <remote
/// ref> <remote sha>`, a deletion included. Lines of another shape are skipped.
pub fn pushed_refs(stdin: &str) -> Vec<String> {
    stdin
        .lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            match fields.as_slice() {
                [_, _, remote, _] => Some((*remote).to_owned()),
                _ => None,
            }
        })
        .collect()
}

/// The remote a push goes to, as git hands it to its pre-push hook: the remote's name (or the URL,
/// for a push to one) and the URL it pushes to.
#[derive(Debug, Clone, Copy, Default)]
pub struct PushedTo<'a> {
    /// The remote's name, or the URL pushed to when the push named no remote.
    pub remote: Option<&'a str>,
    /// The URL pushed to.
    pub url: Option<&'a str>,
}

/// Checks the push `stdin` describes, to `to`.
///
/// Protected branches are judged first, from the settings and the member this machine remembers,
/// without the server, so a server that cannot be reached does not let a push to one through; only
/// a machine that remembers nothing yet asks the server for them. Freezes need the slots' state,
/// so they are judged only when the server answers.
///
/// # Errors
/// A refusal naming who may push, when a ref pushed to is a protected branch or tag the member may
/// not push to; a refusal naming the slot, its holder, why they hold it and until when, when a ref
/// pushed to is frozen by a slot another member holds.
pub async fn check_push(stdin: &str, to: PushedTo<'_>, ctx: &Context) -> Result<(), Failure> {
    let refs = pushed_refs(stdin);
    if refs.is_empty() || switched_off(ctx.var(OFF_VAR).as_deref()) {
        return Ok(());
    }
    let workspaces = ClientWorkspaces::new(ctx.env().clone());
    let workspace = match workspaces.open(&ctx.cwd) {
        Ok(Some(workspace)) => workspace,
        Ok(None) => return Ok(()),
        Err(error) => {
            return let_through(ctx, &Failure::config("invalid_checkout", error.to_string()));
        }
    };
    let remembered = workspace
        .cache()
        .settings()
        .ok()
        .flatten()
        .map(|cached| cached.value);
    if let Some(known) = &remembered
        && let Some(refusal) =
            protected(&*workspace, ctx, &known.settings, &known.member, to, &refs)
    {
        return Err(counted(
            &*workspace,
            Tally::PushNeedsPermission,
            "needs_permission",
            refusal,
        ));
    }
    let connection = match ctx.connect().await {
        Ok(connection) => connection,
        Err(failure) => return let_through(ctx, &failure),
    };
    let governed = match Governed::of(&connection, &connection.me) {
        Ok(governed) => governed,
        Err(failure) => return let_through(ctx, &failure),
    };
    if remembered.is_none()
        && let Some(refusal) = protected(
            &*workspace,
            ctx,
            governed.settings(),
            &connection.me.member.handle,
            to,
            &refs,
        )
    {
        return Err(counted(
            &*workspace,
            Tally::PushNeedsPermission,
            "needs_permission",
            refusal,
        ));
    }
    match frozen(&connection, &governed, &refs).await {
        Ok(Some(refusal)) => Err(counted(&*workspace, Tally::PushFrozen, "frozen", refusal)),
        Ok(None) => Ok(()),
        Err(failure) => let_through(ctx, &failure),
    }
}

/// The refusal of the push, when a ref it goes to is a protected branch or tag of the repository of
/// the remote pushed to that `member` may not push to. Without the remote, the push is judged as one to
/// the repository the checkout governs.
fn protected(
    workspace: &dyn Workspace,
    ctx: &Context,
    settings: &ProjectSettings,
    member: &Handle,
    to: PushedTo<'_>,
    refs: &[String],
) -> Option<String> {
    let facts = workspace.git_facts(&ctx.cwd);
    let here = |dir: &str| workspace.contains_dir(&ctx.cwd, dir);
    let protection = PushProtection {
        pusher: member,
        settings,
        facts: facts.as_ref(),
        here: &here,
    };
    if !protection.applies() {
        return None;
    }
    let governing = workspace
        .repo(settings)
        .ok()
        .and_then(|repo| settings.repo(&repo));
    let CommandVerdict::NeedsPermission {
        target,
        pushers,
        certain,
    } = protection.refs_refusal(to.remote, to.url, refs, governing)?
    else {
        return None;
    };
    Some(wording::push_needs_permission(&target, &pushers, certain))
}

/// The branches of `repo` among the refs pushed to.
fn targets(repo: &RepoId, refs: &[String]) -> Vec<RepoRef> {
    refs.iter()
        .filter_map(|name| RepoRef::of(repo.clone(), name))
        .collect()
}

/// The refusal of the push, when a ref it goes to is frozen by a slot another member holds.
async fn frozen(
    connection: &Connection,
    governed: &Governed,
    refs: &[String],
) -> Result<Option<String>, Failure> {
    let targets = targets(governed.repo(), refs);
    let catalog = governed.settings().slots();
    let freezing: Vec<SlotName> = catalog
        .slots()
        .iter()
        .filter(|spec| targets.iter().any(|target| spec.freezes.contains(target)))
        .map(|spec| spec.name.clone())
        .collect();
    if freezing.is_empty() {
        return Ok(None);
    }
    let mut states = Vec::with_capacity(freezing.len());
    for name in &freezing {
        let answer = connection
            .client
            .slot(name)
            .await
            .map_err(|error| connection.failed(error))?;
        states.push(views::slot_state(&answer.slot));
    }
    let known = KnownBoard::of(&states, &connection.me.member.handle);
    let verdict = push_verdict(&targets, catalog, &known.board, known.me);
    let CommandVerdict::Frozen {
        slot,
        holder,
        purpose,
        until,
        target,
    } = verdict
    else {
        return Ok(None);
    };
    let hold = wording::HoldShown {
        slot: &slot,
        holder: known.handle(holder),
        purpose: &purpose,
        until,
    };
    Ok(Some(format!(
        "{} To push anyway, knowing what it would break: git push --no-verify.",
        wording::frozen(&slot, &hold, &target.to_string())
    )))
}

/// The refusal, counted on this machine as `tally` and ending with the id it is counted under.
fn counted(workspace: &dyn Workspace, tally: Tally, code: &str, refusal: String) -> Failure {
    let day = ReportDay::of(OffsetDateTime::now_utc());
    let message = match workspace.tallies().refused(day, &[tally]) {
        Some(id) => format!("{refusal} {}", wording::refusal_named(&id)),
        None => refusal,
    };
    Failure::refused(code, message)
}

/// Lets the push go, saying once why it was not checked.
fn let_through(ctx: &Context, failure: &Failure) -> Result<(), Failure> {
    ctx.printer.warn(&format!(
        "dibs did not check this push, so it goes ahead: {}",
        failure.message
    ));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_remote_refs_are_read_from_each_line_git_writes() {
        let stdin = "refs/heads/feature 1111111 refs/heads/main 2222222\n\
                     (delete) 0000000 refs/heads/old 3333333\n\
                     \n\
                     not a line of git's\n";
        assert_eq!(pushed_refs(stdin), ["refs/heads/main", "refs/heads/old"]);
        assert!(pushed_refs("").is_empty());
    }
}

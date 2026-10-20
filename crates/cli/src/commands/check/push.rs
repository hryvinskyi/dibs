//! `dibs check --push`: the push git describes to its pre-push hook, judged by the rule the agents'
//! command check uses. It knows the exact refs pushed to whatever the command line said and
//! whoever typed it, so it covers a push from a terminal and one with no refspec.
//!
//! It refuses only on a verdict: a ref frozen by a slot another member holds. Anything else that
//! keeps it from deciding — no checkout, no token, a server that cannot be reached — lets the push
//! go with one warning; `git push --no-verify` stays the conscious way past a refusal.

use dibs_agent_host::slots::KnownBoard;
use dibs_agent_host::wording;
use dibs_domain::repo::RepoId;
use dibs_domain::slot::{CommandVerdict, RepoRef, SlotName, push_verdict};

use crate::commands::check::Governed;
use crate::context::{Connection, Context};
use crate::failure::Failure;
use crate::guard::views;

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

/// Checks the push `stdin` describes.
///
/// # Errors
/// A refusal naming the slot, its holder, why they hold it and until when, when a ref pushed to is
/// frozen by a slot another member holds.
pub async fn check_push(stdin: &str, ctx: &Context) -> Result<(), Failure> {
    let refs = pushed_refs(stdin);
    if refs.is_empty() {
        return Ok(());
    }
    match ctx.checkout() {
        Ok(Some(_)) => {}
        Ok(None) => return Ok(()),
        Err(failure) => return let_through(ctx, &failure),
    }
    let connection = match ctx.connect().await {
        Ok(connection) => connection,
        Err(failure) => return let_through(ctx, &failure),
    };
    let governed = match Governed::of(&connection, &connection.me) {
        Ok(governed) => governed,
        Err(failure) => return let_through(ctx, &failure),
    };
    match frozen(&connection, &governed, &refs).await {
        Ok(Some(refusal)) => Err(Failure::refused("frozen", refusal)),
        Ok(None) => Ok(()),
        Err(failure) => let_through(ctx, &failure),
    }
}

/// The refusal of the push, when a ref it goes to is frozen by a slot another member holds.
async fn frozen(
    connection: &Connection,
    governed: &Governed,
    refs: &[String],
) -> Result<Option<String>, Failure> {
    let repo: &RepoId = governed.repo();
    let targets: Vec<RepoRef> = refs
        .iter()
        .filter_map(|name| RepoRef::of(repo.clone(), name))
        .collect();
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

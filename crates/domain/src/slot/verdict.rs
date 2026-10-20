//! What a command line needs before it may run: decided on the member's machine, from the simple
//! commands of the line, the settings and the state of the slots it touches. The line itself never
//! leaves the machine.
//!
//! The order: every `git push` is checked first against the protected branches of the repository
//! it pushes to (`NeedsPermission`: a pull request is the way on), failing closed where its branches
//! cannot be read; then a push whose target ref can be read against the refs a slot held by another
//! member freezes (`Frozen`: waiting is the way on, not taking the slot), on a best-effort reading;
//! then the first declared slot whose pattern matches any simple command decides (`NeedsSlot`, or `Holding` while
//! the member holds it, unless the slot runs only inside `dibs slot run` and the hold is not kept
//! by a run or a workflow); with no match the line is `Free`.

use std::collections::BTreeSet;

use time::OffsetDateTime;

use crate::handle::Handle;
use crate::ids::MemberId;
use crate::push::{PushProtection, git_pushes};
use crate::repo::RepoId;
use crate::slot::command::SimpleCommand;
use crate::slot::spec::{Purpose, RepoRef, SlotCatalog, SlotName, SlotSpec};
use crate::slot::state::{Liveness, SlotBoard, SlotHold};

/// What a command line needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandVerdict {
    /// Nothing: no slot governs it.
    Free,
    /// A push that updates, or may update, a protected branch the member is not among the pushers
    /// of; a pull request is the way there.
    NeedsPermission {
        /// The branch.
        target: RepoRef,
        /// The members who may push to it, by handle.
        pushers: BTreeSet<Handle>,
        /// Whether the push names that branch; `false` when it pushes every branch, or which ones
        /// it pushes cannot be read.
        certain: bool,
    },
    /// A push to a ref another member's held slot freezes.
    Frozen {
        /// The slot.
        slot: SlotName,
        /// Its holder.
        holder: MemberId,
        /// Why they hold it.
        purpose: Purpose,
        /// When the hold ends unless renewed.
        until: OffsetDateTime,
        /// The ref pushed.
        target: RepoRef,
    },
    /// The command needs a slot the member does not hold (or, for a slot that runs only inside
    /// `dibs slot run`, holds only through a plain take).
    NeedsSlot {
        /// The slot.
        slot: SlotName,
        /// The hold of its group, when someone holds it.
        holder: Option<SlotHold>,
        /// How many members wait for its group.
        waiting: usize,
        /// Whether its group is on offer to the member.
        offered_to_me: bool,
        /// Whether its commands run only inside `dibs slot run`.
        run_only: bool,
    },
    /// The member holds the slot the command needs.
    Holding {
        /// The slot.
        slot: SlotName,
    },
}

/// Decides what the commands of one line need, for `me` in `repo`. `protection` judges pushes
/// against protected branches; without it, no branch counts as protected.
pub fn command_verdict(
    commands: &[SimpleCommand],
    repo: &RepoId,
    catalog: &SlotCatalog,
    board: &SlotBoard,
    me: MemberId,
    protection: Option<&PushProtection<'_>>,
) -> CommandVerdict {
    let governed: Vec<&SimpleCommand> = commands.iter().filter(|c| !is_slot_wrapper(c)).collect();
    if let Some(refusal) =
        protection.and_then(|protection| protection.refusal(&git_pushes(&governed)))
    {
        return refusal;
    }
    let targets = pushed_refs(&governed, repo);
    let pushed = push_verdict(&targets, catalog, board, me);
    if pushed != CommandVerdict::Free {
        return pushed;
    }
    let Some(spec) = catalog
        .slots()
        .iter()
        .find(|spec| governed.iter().any(|command| spec.matches(&command.text)))
    else {
        return CommandVerdict::Free;
    };
    let key = spec.group_key();
    let mine = board.hold(&spec.name).filter(|hold| hold.member == me);
    if let Some(hold) = mine
        && !(spec.run_only && hold.liveness == Liveness::Session)
    {
        return CommandVerdict::Holding {
            slot: spec.name.clone(),
        };
    }
    CommandVerdict::NeedsSlot {
        slot: spec.name.clone(),
        holder: board.group_holds(catalog, &key).next().cloned(),
        waiting: board.group_queue(catalog, &key).len(),
        offered_to_me: board
            .group_offer(catalog, &key)
            .is_some_and(|offer| offer.member == me),
        run_only: spec.run_only,
    }
}

/// The slots whose state [`command_verdict`] reads for these commands: each slot that freezes a ref
/// a push among them targets, then the first slot whose pattern matches one of them, each named
/// once. Empty when the line touches no slot: its verdict is then `Free` whatever the slots' state,
/// so nobody needs to be asked about it.
pub fn slots_in_question(
    commands: &[SimpleCommand],
    repo: &RepoId,
    catalog: &SlotCatalog,
) -> Vec<SlotName> {
    let governed: Vec<&SimpleCommand> = commands.iter().filter(|c| !is_slot_wrapper(c)).collect();
    let targets = pushed_refs(&governed, repo);
    let mut names: Vec<SlotName> = catalog
        .slots()
        .iter()
        .filter(|spec| targets.iter().any(|target| spec.freezes.contains(target)))
        .map(|spec| spec.name.clone())
        .collect();
    if let Some(spec) = catalog
        .slots()
        .iter()
        .find(|spec| governed.iter().any(|command| spec.matches(&command.text)))
        && !names.contains(&spec.name)
    {
        names.push(spec.name.clone());
    }
    names
}

/// What a push of `targets` needs from `me` once no protected branch refused it:
/// [`CommandVerdict::Frozen`] for the first target a slot held by another member freezes,
/// [`CommandVerdict::Free`] otherwise. A hold of `me` never freezes their own push. The agent's
/// command check and git's pre-push check both decide here and in [`Pusher::refusal`], so a push is
/// judged the same whoever typed it.
pub fn push_verdict(
    targets: &[RepoRef],
    catalog: &SlotCatalog,
    board: &SlotBoard,
    me: MemberId,
) -> CommandVerdict {
    for target in targets {
        if let Some((spec, hold)) = freezing(catalog, board, target, me) {
            return CommandVerdict::Frozen {
                slot: spec.name.clone(),
                holder: hold.member,
                purpose: hold.purpose.clone(),
                until: hold.expires_at,
                target: target.clone(),
            };
        }
    }
    CommandVerdict::Free
}

/// The branches the `git push` commands among `commands` name, in `repo`, as far as the line
/// alone reads them.
fn pushed_refs(commands: &[&SimpleCommand], repo: &RepoId) -> Vec<RepoRef> {
    commands
        .iter()
        .filter_map(|command| push_targets(command, repo))
        .flatten()
        .collect()
}

/// The slot held by another member that freezes `target`, with its hold.
fn freezing<'a>(
    catalog: &'a SlotCatalog,
    board: &'a SlotBoard,
    target: &RepoRef,
    me: MemberId,
) -> Option<(&'a SlotSpec, &'a SlotHold)> {
    board
        .holds
        .iter()
        .filter(|hold| hold.member != me)
        .find_map(|hold| {
            let spec = catalog.get(&hold.slot)?;
            spec.freezes.contains(target).then_some((spec, hold))
        })
}

/// Whether the command is `dibs slot …`, which takes the slot itself.
fn is_slot_wrapper(command: &SimpleCommand) -> bool {
    let name = command.words.first().map(|word| program_name(word));
    name == Some("dibs") && command.words.get(1).map(String::as_str) == Some("slot")
}

fn program_name(word: &str) -> &str {
    let base = word.rsplit(['/', '\\']).next().unwrap_or(word);
    base.strip_suffix(".exe").unwrap_or(base)
}

/// The branch refs a `git push` names on its command line, in `repo`; `None` for any other command
/// and for a push whose targets cannot be read from it (another checkout through `-C`, no refspec,
/// `--all`, `--mirror`, `HEAD`).
pub fn push_targets(command: &SimpleCommand, repo: &RepoId) -> Option<Vec<RepoRef>> {
    let mut words = command.words.iter().map(String::as_str);
    if words.next().map(program_name) != Some("git") {
        return None;
    }
    loop {
        match words.next()? {
            "push" => break,
            "-C" | "--git-dir" | "--work-tree" => return None,
            "-c" | "--namespace" => {
                words.next()?;
            }
            option if option.starts_with('-') => {
                if option.starts_with("--git-dir=") || option.starts_with("--work-tree=") {
                    return None;
                }
            }
            _ => return None,
        }
    }
    let mut positional = Vec::new();
    while let Some(word) = words.next() {
        match word {
            "--all" | "--mirror" | "--tags" | "--branches" => return None,
            "--repo" | "-o" | "--push-option" | "--receive-pack" | "--exec" => {
                words.next()?;
            }
            option if option.starts_with('-') => {}
            other => positional.push(other),
        }
    }
    let refspecs = positional.get(1..)?;
    if refspecs.is_empty() {
        return None;
    }
    let mut targets = Vec::new();
    for refspec in refspecs {
        let refspec = refspec.strip_prefix('+').unwrap_or(refspec);
        let target = refspec.rsplit_once(':').map_or(refspec, |(_, dst)| dst);
        if target.is_empty() || target == "HEAD" || target.contains('*') {
            return None;
        }
        let name = if target.starts_with("refs/") {
            target.to_owned()
        } else {
            format!("refs/heads/{target}")
        };
        if let Some(found) = RepoRef::of(repo.clone(), &name) {
            targets.push(found);
        }
    }
    Some(targets)
}

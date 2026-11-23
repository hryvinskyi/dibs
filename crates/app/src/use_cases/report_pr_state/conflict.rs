//! What dibs writes to a claim's holder when a pull request the claim records does not merge
//! cleanly into its base: what to do about it, and the recipe of every regenerate rule of the pull
//! request's repository that names one.

use time::OffsetDateTime;

use dibs_domain::claim::Claim;
use dibs_domain::hot_rule::Recipe;
use dibs_domain::message::{Body, Importance};
use dibs_domain::paths::PathGlob;
use dibs_domain::ref_scheme::RefSchemeRegistry;
use dibs_domain::review::PrRef;
use dibs_domain::settings::RepoSpec;

use crate::error::AppError;
use crate::named::ItemName;
use crate::ports::IdGen;
use crate::use_cases::claim_notice::ClaimNotice;
use crate::use_cases::common::existing_item;
use crate::use_cases::writing::{TITLE_NOTE, titled_body};
use crate::work::Work;

/// The most recipes a conflict message lists; it says how many more there are.
pub(super) const LISTED_RECIPES: usize = 20;

/// The most globs of one rule a conflict message lists; it says how many more there are.
const LISTED_GLOBS: usize = 5;

/// What the holder does about a pull request that does not merge cleanly.
const CONFLICT_FIX: &str = "To fix it, merge the base into the pull request's branch or rebase the branch onto the base, resolve the conflicts and push; or merge it by hand. Until then it cannot be merged.";

/// The lead-in of the recipes a conflict message lists.
const RECIPES_LEAD: &str = "Files under a regenerate rule are not merged by hand: take them from the base and regenerate them.";

/// The bytes kept for the line that says how many recipes are not listed.
const MORE_ROOM: usize = 64;

/// The text the holder of a claim is told when a pull request it records does not merge cleanly.
pub(crate) struct ConflictNotice<'a> {
    /// Ids for the message, and the conversation when the claim has none.
    pub(crate) ids: &'a dyn IdGen,
    /// How items are named.
    pub(crate) schemes: &'a RefSchemeRegistry,
    /// When it is told.
    pub(crate) now: OffsetDateTime,
}

impl ConflictNotice<'_> {
    /// Tells the holder of `claim`, in the claim's conversation and at high importance, that `pr`
    /// does not merge cleanly into its base, with the recipes of `repo`, the pull request's
    /// repository, when the settings could be read and list it.
    pub(crate) async fn tell(
        &self,
        work: &mut Work,
        claim: &Claim,
        pr: &PrRef,
        repo: Option<&RepoSpec>,
    ) -> Result<(), AppError> {
        let item = ItemName::of(&existing_item(work, claim.item).await?, self.schemes);
        ClaimNotice {
            claim,
            item: &item,
            body: titled_body(&conflict_text(pr, &item, repo), None)?,
            importance: Importance::High,
        }
        .send(work, self.ids, self.now)
        .await?;
        Ok(())
    }
}

/// The message's own sentences: the conflict and what to do about it, then, when `repo` has
/// regenerate rules with a recipe, one line per rule up to [`LISTED_RECIPES`] and as many as fit
/// one message beside the note on titles, and a line saying how many more there are.
pub(super) fn conflict_text(pr: &PrRef, item: &ItemName, repo: Option<&RepoSpec>) -> String {
    let mut text = format!(
        "Pull request {pr}, recorded on your claim on work item {} \"{}\", does not merge cleanly into its base. {CONFLICT_FIX}",
        item.label(),
        item.title
    );
    let recipes: Vec<(&[PathGlob], &Recipe)> = repo
        .map(|repo| repo.recipes().collect())
        .unwrap_or_default();
    if recipes.is_empty() {
        return text;
    }
    text.push('\n');
    text.push_str(RECIPES_LEAD);
    let room = Body::MAX_BYTES.saturating_sub(TITLE_NOTE.len() + 1 + MORE_ROOM);
    let mut listed = 0;
    for (paths, recipe) in recipes.iter().take(LISTED_RECIPES) {
        let line = format!("\n- {}: regenerate with `{recipe}`", globs(paths));
        if text.len() + line.len() > room {
            break;
        }
        text.push_str(&line);
        listed += 1;
    }
    match recipes.len() - listed {
        0 => {}
        1 => text.push_str("\n- and 1 more rule with a recipe"),
        more => text.push_str(&format!("\n- and {more} more rules with a recipe")),
    }
    text
}

/// A rule's globs for a line: the first [`LISTED_GLOBS`], then how many more.
fn globs(paths: &[PathGlob]) -> String {
    let named: Vec<&str> = paths
        .iter()
        .take(LISTED_GLOBS)
        .map(PathGlob::as_str)
        .collect();
    let more = paths.len().saturating_sub(LISTED_GLOBS);
    if more == 0 {
        named.join(", ")
    } else {
        format!("{} and {more} more", named.join(", "))
    }
}

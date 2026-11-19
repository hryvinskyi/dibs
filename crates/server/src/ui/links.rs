//! The link views the pages print references with: a ticket, a pull request, a branch, a file, an
//! area, a person, a conversation, and a member's free text with the references found in it. Each
//! view is plain data the templates print — a label, a title, an address in this service, an
//! address outside it — and never markup.
//!
//! An item has a place in this service while a live or stale claim holds it (Claims), or while it
//! waits in the queue: in the ready list, or left out of it for another reason than a claim
//! (Queue). A ticket links there when it has a place, with the tracker's page beside it; else to
//! the tracker's page; else to the search for its name.
//!
//! An address outside this service is only ever `https://` or `http://`, in any case: every other
//! address a reference scheme, a forge or a scanned text gives is treated as no address, so the view
//! falls back to the search or to plain text.

use std::collections::BTreeMap;
use std::sync::Arc;

use dibs_app::use_cases::StatusView;
use dibs_domain::area::QualifiedArea;
use dibs_domain::forge::{Forge, ForgeRegistry, Revision};
use dibs_domain::handle::Handle;
use dibs_domain::ids::{ItemId, ThreadId};
use dibs_domain::item::ExternalRef;
use dibs_domain::paths::RepoPath;
use dibs_domain::radar::BranchName;
use dibs_domain::ref_scheme::RefSchemeRegistry;
use dibs_domain::repo::{RemoteUrl, RepoId};
use dibs_domain::review::PrRef;
use dibs_domain::settings::ProjectSettings;
use dibs_domain::text_refs::{RefContext, TextRef, scan};

use crate::ui::queue::listing::waits;
use crate::ui::rows::reference;

/// How the arrow beside a ticket names the page it leads to.
const TRACKER: &str = "the tracker";

/// The characters after which a handle's second initial is taken.
const NAME_SEPARATORS: &[char] = &['-', '_', '.'];

/// How many colours a person's avatar is drawn in.
const COLOURS: u32 = 6;

/// Where an item has a page of its own in this service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    /// On Claims: a live or stale claim holds it.
    Claims,
    /// On Queue: it waits to be claimed.
    Queue,
}

impl Place {
    /// The address of the item's page there.
    fn href(self, id: ItemId) -> String {
        match self {
            Self::Claims => format!("/ui/claims/{id}"),
            Self::Queue => format!("/ui/queue/{id}"),
        }
    }
}

/// The place of every item the status shows, with its reference: the claimed items first, then the
/// ready list, then the items left out of it for another reason than a claim.
pub fn places(status: &StatusView) -> Vec<(ItemId, Option<ExternalRef>, Place)> {
    let claimed = status
        .live
        .iter()
        .chain(&status.stale)
        .map(|held| (&held.item.item, Place::Claims));
    let ready = status
        .ready
        .ready
        .iter()
        .map(|entry| (&entry.item.item, Place::Queue));
    let waiting = status
        .ready
        .excluded
        .iter()
        .filter(|entry| waits(entry))
        .map(|entry| (&entry.item.item, Place::Queue));
    claimed
        .chain(ready)
        .chain(waiting)
        .map(|(item, place)| (item.id, item.external_ref.clone(), place))
        .collect()
}

/// An item with a place in this service.
#[derive(Debug, Clone)]
struct Placed {
    reference: Option<ExternalRef>,
    href: String,
}

/// What the link views of one request are built from: the project's settings, the reference
/// schemes and forges the deployment knows, and the place of each item the page knows.
#[derive(Debug, Clone)]
pub struct LinkContext {
    settings: ProjectSettings,
    schemes: Arc<RefSchemeRegistry>,
    forges: Arc<ForgeRegistry>,
    places: BTreeMap<ItemId, Placed>,
}

impl LinkContext {
    /// The context of these settings and registries, placing the items given; the first place
    /// given for an item stands.
    pub fn new(
        settings: ProjectSettings,
        schemes: Arc<RefSchemeRegistry>,
        forges: Arc<ForgeRegistry>,
        places: impl IntoIterator<Item = (ItemId, Option<ExternalRef>, Place)>,
    ) -> Self {
        let mut placed = BTreeMap::new();
        for (id, reference, place) in places {
            placed.entry(id).or_insert_with(|| Placed {
                reference,
                href: place.href(id),
            });
        }
        Self {
            settings,
            schemes,
            forges,
            places: placed,
        }
    }

    /// The tracker's page of a reference, when its scheme has pages at a web address.
    fn tracker(&self, external: &ExternalRef) -> Option<Link> {
        self.schemes.link(external).and_then(Link::outside)
    }

    /// Where a ticket named by its reference leads: its item's place, else the tracker's page, else
    /// the search for it.
    fn ticket(&self, external: &ExternalRef) -> Link {
        match self
            .places
            .values()
            .find(|placed| placed.reference.as_ref() == Some(external))
        {
            Some(placed) => Link::inside(placed.href.clone()),
            None => self
                .tracker(external)
                .unwrap_or_else(|| Link::search(&external.to_string())),
        }
    }

    /// A forge's page of a repository of the project, when the repository has a remote on a forge
    /// the deployment knows and the page is at a web address.
    fn forge_page(
        &self,
        repo: &RepoId,
        page: impl FnOnce(&dyn Forge, &RemoteUrl) -> String,
    ) -> Option<Link> {
        let remote = self.settings.repo(repo)?.remote.as_ref()?;
        let forge = self.forges.of(remote)?;
        Link::outside(page(forge, remote))
    }

    /// The repository a reference names, when its scheme names one.
    pub fn repository_of(&self, external: &ExternalRef) -> Option<RemoteUrl> {
        self.schemes.repository_of(external)
    }

    /// Where `#{number}` of a repository of the project leads, as it is linked in a text about that
    /// repository: the forge's page of that issue, else the search for `#{number}`.
    pub fn numbered(&self, repo: &RepoId, number: u64) -> Link {
        self.forge_page(repo, |forge, remote| forge.issue(remote, number))
            .unwrap_or_else(|| Link::search(&format!("#{number}")))
    }

    /// Where a reference found in a text leads, when anywhere.
    fn found(&self, found: &TextRef) -> Option<Link> {
        match found {
            TextRef::Url(address) => Link::outside(address.clone()),
            TextRef::Ticket(external) => Some(self.ticket(external)),
            TextRef::Numbered { repo, number } => {
                self.forge_page(repo, |forge, remote| forge.issue(remote, *number))
            }
            TextRef::Area(area) => Some(Link::inside(area_href(area))),
            TextRef::Commit { repo, sha } => {
                self.forge_page(repo, |forge, remote| forge.commit(remote, sha))
            }
        }
    }
}

/// An address a link leads to: in this service, or outside it in a new tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    /// The address.
    pub href: String,
    /// Whether it is outside this service.
    pub outside: bool,
}

impl Link {
    /// A link to a page of this service.
    fn inside(href: String) -> Self {
        Self {
            href,
            outside: false,
        }
    }

    /// A link to a page outside this service, when the address is a web address; any other
    /// address is no link.
    fn outside(address: String) -> Option<Self> {
        is_web_address(&address).then_some(Self {
            href: address,
            outside: true,
        })
    }

    /// A link to the search for `value`.
    fn search(value: &str) -> Self {
        Self::inside(format!("/ui/search?q={}", query_value(value)))
    }
}

/// Whether an address starts with `https://` or `http://`, in any case.
fn is_web_address(address: &str) -> bool {
    ["https://", "http://"].iter().any(|prefix| {
        address
            .get(..prefix.len())
            .is_some_and(|start| start.eq_ignore_ascii_case(prefix))
    })
}

/// A query-string value with every byte outside the unreserved set percent-encoded, so an address
/// built with it is visible ASCII.
pub fn query_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// The address of an area's page.
fn area_href(area: &QualifiedArea) -> String {
    format!("/ui/areas/{area}")
}

/// A ticket: an item named by its reference, or by its id when it has none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TicketView {
    /// How it is named: `{repository}#{number}` for a reference whose scheme names a repository and
    /// whose value ends in a number, the reference whole otherwise, `#{id}` without one.
    pub label: String,
    /// Its full name: the reference, or `#{id}`.
    pub title: String,
    /// Where its name leads.
    pub main: Link,
    /// The tracker's page, beside an in-app main link.
    pub out: Option<Link>,
    /// What the page `out` leads to is called.
    pub out_name: &'static str,
}

impl TicketView {
    /// The ticket of the item `id` with this reference.
    pub fn of(id: ItemId, external: Option<&ExternalRef>, context: &LinkContext) -> Self {
        let title = reference(id, external);
        let label = external
            .and_then(|external| short_label(external, &context.schemes))
            .unwrap_or_else(|| title.clone());
        let tracker = external.and_then(|external| context.tracker(external));
        let (main, out) = match context.places.get(&id) {
            Some(placed) => (Link::inside(placed.href.clone()), tracker),
            None => (tracker.unwrap_or_else(|| Link::search(&title)), None),
        };
        Self {
            label,
            title,
            main,
            out,
            out_name: TRACKER,
        }
    }

    /// The ticket of the item `id`, named as the context knows it; an item it does not know is
    /// named by its id.
    pub fn of_item(id: ItemId, context: &LinkContext) -> Self {
        let external = context
            .places
            .get(&id)
            .and_then(|placed| placed.reference.clone());
        Self::of(id, external.as_ref(), context)
    }
}

/// `{repository}#{number}` for a reference whose scheme names a repository and whose value ends in
/// `#` and a number; the repository is the last segment of its path.
fn short_label(external: &ExternalRef, schemes: &RefSchemeRegistry) -> Option<String> {
    let repository = schemes.repository_of(external)?;
    let (_, number) = external.value().rsplit_once('#')?;
    if number.is_empty() || !number.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let name = repository
        .path()
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())?;
    Some(format!("{name}#{number}"))
}

/// A member: their handle, the initials and colour of their avatar, and their claims' address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonView {
    /// The handle.
    pub handle: String,
    /// One or two upper-case letters.
    pub initials: String,
    /// The avatar's colour, 1 to 6, the same for one handle wherever it is shown.
    pub colour: u8,
    /// The address of their claims.
    pub href: String,
}

impl PersonView {
    /// The view of the member with this handle.
    pub fn of(handle: &Handle) -> Self {
        let handle = handle.as_str();
        Self {
            handle: handle.to_owned(),
            initials: initials(handle),
            colour: colour(handle),
            href: format!("/ui/claims?who={}", query_value(handle)),
        }
    }
}

/// An area of a repository of the project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AreaView {
    /// Its name, as a row shows it.
    pub label: String,
    /// `repo:name`, as it is shown alone.
    pub full: String,
    /// The address of its page.
    pub href: String,
}

impl AreaView {
    /// The view of an area.
    pub fn of(area: &QualifiedArea) -> Self {
        Self {
            label: area.name.to_string(),
            full: area.to_string(),
            href: area_href(area),
        }
    }
}

/// A page outside this service shown beside an in-app link, and what that page is called.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Beside {
    /// Where it leads.
    pub link: Link,
    /// What the page is called, such as the forge's name.
    pub name: &'static str,
}

/// A pull request of a forge repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequestView {
    /// `{name}#{number}`: the repository's last path segment and the number.
    pub label: String,
    /// Its full name, `{owner}/{name}#{number}`.
    pub title: String,
    /// Where its name leads.
    pub main: Link,
    /// The forge's page, beside an in-app main link.
    pub out: Option<Beside>,
}

impl PullRequestView {
    /// The view of `pr`. Its name leads to `here` when given — the claim in review with it —
    /// else to the forge's page of the pull request, else to the search for it.
    ///
    /// The forge's page is that of the project's repository whose remote's path is the pull
    /// request's; when the settings name no such remote, that of `item_repo` — the repository the
    /// reference of the claim's item names — when its path is the pull request's. A forge is
    /// never assumed from the pull request's text alone.
    pub fn of(
        pr: &PrRef,
        here: Option<String>,
        item_repo: Option<&RemoteUrl>,
        context: &LinkContext,
    ) -> Self {
        let path = pr.repo_path();
        let remote = context
            .settings
            .repo_of_pr(pr)
            .and_then(|spec| spec.remote.as_ref())
            .filter(|remote| remote.path() == path)
            .or_else(|| item_repo.filter(|remote| remote.path() == path));
        let forge = remote.and_then(|remote| {
            let forge = context.forges.of(remote)?;
            let link = Link::outside(forge.pull_request(remote, pr.number()))?;
            Some(Beside {
                link,
                name: forge.name(),
            })
        });
        let name = path.rsplit('/').next().unwrap_or(path);
        let title = pr.to_string();
        let (main, out) = match (here, forge) {
            (Some(here), forge) => (Link::inside(here), forge),
            (None, Some(forge)) => (forge.link, None),
            (None, None) => (Link::search(&title), None),
        };
        Self {
            label: format!("{name}#{}", pr.number()),
            title,
            main,
            out,
        }
    }
}

/// A branch of a repository of the project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchView {
    /// The branch's name.
    pub label: String,
    /// Where it leads.
    pub main: Link,
}

impl BranchView {
    /// The view of `branch` in `repo`: the forge's page of the branch, else the claims on it.
    pub fn of(repo: &RepoId, branch: &BranchName, context: &LinkContext) -> Self {
        let label = branch.to_string();
        let main = context
            .forge_page(repo, |forge, remote| forge.branch(remote, branch))
            .unwrap_or_else(|| Link::inside(format!("/ui/claims?q={}", query_value(&label))));
        Self { label, main }
    }
}

/// A file of a repository of the project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileView {
    /// The path inside its repository.
    pub label: String,
    /// Its full name, `repo:path`.
    pub title: String,
    /// Where it leads.
    pub main: Link,
}

impl FileView {
    /// The view of a file at a revision: the forge's page of the file there, else the search for
    /// its full name.
    pub fn of(file: &RepoPath, at: Revision<'_>, context: &LinkContext) -> Self {
        let title = file.to_string();
        let main = context
            .forge_page(&file.repo, |forge, remote| {
                forge.file(remote, at, file.path.as_str())
            })
            .unwrap_or_else(|| Link::search(&title));
        Self {
            label: file.path.to_string(),
            title,
            main,
        }
    }
}

/// A directory of a repository of the project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryView {
    /// The directory as written, or `/` for the repository's root.
    pub label: String,
    /// Its full name, `repo:directory`, or `repo:/` for the root.
    pub title: String,
    /// Where it leads.
    pub main: Link,
}

impl DirectoryView {
    /// The view of `directory` of `repo` at its default branch: the forge's page of the directory
    /// there, else the search for its full name. An empty `directory` is the repository's root.
    pub fn of(repo: &RepoId, directory: &str, context: &LinkContext) -> Self {
        let label = if directory.is_empty() { "/" } else { directory };
        let title = format!("{repo}:{label}");
        let main = context
            .forge_page(repo, |forge, remote| {
                forge.directory(remote, Revision::DefaultBranch, directory)
            })
            .unwrap_or_else(|| Link::search(&title));
        Self {
            label: label.to_owned(),
            title,
            main,
        }
    }
}

/// A conversation: its subject, and the address of its page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationView {
    /// The subject, with the references in it linked.
    pub subject: LinkedText,
    /// The address of the conversation.
    pub href: String,
}

impl ConversationView {
    /// The view of the conversation `thread` with this subject.
    pub fn of(thread: ThreadId, subject: &str, context: &LinkContext) -> Self {
        Self {
            subject: LinkedText::of(subject, None, context),
            href: format!("/ui/inbox/{thread}"),
        }
    }
}

/// The handle's first character, then the first after a `-`, `_` or `.`, or else its next character
/// that is none of those; upper-cased.
fn initials(handle: &str) -> String {
    let chars: Vec<char> = handle.chars().collect();
    let Some(first) = chars.first() else {
        return String::new();
    };
    let separated = chars
        .windows(2)
        .find(|pair| NAME_SEPARATORS.contains(&pair[0]) && !NAME_SEPARATORS.contains(&pair[1]))
        .map(|pair| pair[1]);
    let second = separated.or_else(|| {
        chars
            .iter()
            .skip(1)
            .find(|c| !NAME_SEPARATORS.contains(c))
            .copied()
    });
    first
        .to_uppercase()
        .chain(second.into_iter().flat_map(char::to_uppercase))
        .collect()
}

/// A colour from 1 to 6 that depends on the handle alone: FNV-1a over its bytes.
fn colour(handle: &str) -> u8 {
    let hash = handle.bytes().fold(0x811c_9dc5_u32, |hash, byte| {
        (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193)
    });
    u8::try_from(hash % COLOURS).map_or(1, |index| index + 1)
}

/// A member's free text, in pieces: each reference found in it linked where an address can be
/// built, the rest as written. Joined, the pieces' texts are the text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkedText {
    /// The pieces, in order; no two plain pieces are adjacent.
    pub pieces: Vec<LinkedPiece>,
}

/// A piece of a linked text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkedPiece {
    /// The text, exactly as written.
    pub text: String,
    /// Where it leads, when it is a reference with an address.
    pub link: Option<Link>,
}

impl LinkedText {
    /// The text with its references linked; `home` is the repository a bare `#N` or a commit hash
    /// belongs to, when the text is about one.
    ///
    /// An address leads to itself; a ticket where a ticket's name leads; a repository's `#N` or
    /// `owner/name#N` to the forge's page of that issue; an area to its page here; a commit hash to
    /// the forge's page of that commit. A reference no address can be built for stays plain.
    pub fn of(text: &str, home: Option<&RepoId>, context: &LinkContext) -> Self {
        let scanned = scan(
            text,
            &RefContext {
                schemes: &context.schemes,
                settings: &context.settings,
                home,
            },
        );
        let mut pieces: Vec<LinkedPiece> = Vec::with_capacity(scanned.len());
        for piece in scanned {
            let link = piece
                .reference
                .as_ref()
                .and_then(|found| context.found(found));
            match (pieces.last_mut(), link) {
                (Some(last), None) if last.link.is_none() => last.text.push_str(&piece.text),
                (_, link) => pieces.push(LinkedPiece {
                    text: piece.text,
                    link,
                }),
            }
        }
        Self { pieces }
    }
}

#[cfg(test)]
mod tests {
    use dibs_domain::hot_rule::HotRuleRegistry;
    use dibs_domain::ref_scheme::{RefRefused, RefScheme};
    use dibs_domain::settings::SettingsDocument;

    use super::*;

    /// A scheme whose pages are at addresses that are not web addresses.
    #[derive(Debug)]
    struct Hostile;

    impl RefScheme for Hostile {
        fn canonical(&self, value: &str) -> Result<String, RefRefused> {
            Ok(value.to_owned())
        }

        fn link(&self, _: &str) -> Option<String> {
            Some("javascript:alert(1)".to_owned())
        }

        fn repository(&self, _: &str) -> Option<RemoteUrl> {
            None
        }
    }

    fn settings() -> ProjectSettings {
        let document: SettingsDocument = serde_json::from_value(serde_json::json!({
            "repos": [
                {"id": "api", "remote": "https://github.com/acme/widgets.git",
                 "areas": [{"name": "billing", "paths": ["billing/**"]}]},
                {"id": "docs", "remote": "https://git.example.com/acme/docs.git",
                 "areas": [{"name": "guide", "paths": ["guide/**"]}]}
            ]
        }))
        .unwrap();
        ProjectSettings::parse(&document, &HotRuleRegistry::standard()).unwrap()
    }

    fn external(text: &str) -> ExternalRef {
        ExternalRef::parse(text).unwrap()
    }

    fn context(places: Vec<(ItemId, Option<ExternalRef>, Place)>) -> LinkContext {
        LinkContext::new(
            settings(),
            Arc::new(RefSchemeRegistry::standard().with("evil", Box::new(Hostile))),
            Arc::new(ForgeRegistry::standard()),
            places,
        )
    }

    fn inside(href: &str) -> Link {
        Link::inside(href.to_owned())
    }

    fn outside(href: &str) -> Link {
        Link {
            href: href.to_owned(),
            outside: true,
        }
    }

    #[test]
    fn a_ticket_is_labelled_by_its_repository_and_number_or_its_whole_reference_or_its_id() {
        let none = context(Vec::new());
        let gh = external("gh:acme/widgets#42");
        let ticket = TicketView::of(ItemId(7), Some(&gh), &none);
        assert_eq!(ticket.label, "widgets#42");
        assert_eq!(ticket.title, "gh:acme/widgets#42");
        let other = external("jira:ABC-12");
        assert_eq!(
            TicketView::of(ItemId(7), Some(&other), &none).label,
            "jira:ABC-12"
        );
        let bare = TicketView::of(ItemId(7), None, &none);
        assert_eq!((bare.label.as_str(), bare.title.as_str()), ("#7", "#7"));
    }

    #[test]
    fn a_placed_ticket_leads_here_with_the_trackers_page_beside_it() {
        let gh = external("gh:acme/widgets#42");
        let claimed = context(vec![(ItemId(7), Some(gh.clone()), Place::Claims)]);
        let ticket = TicketView::of(ItemId(7), Some(&gh), &claimed);
        assert_eq!(ticket.main, inside("/ui/claims/7"));
        assert_eq!(
            ticket.out,
            Some(outside("https://github.com/acme/widgets/issues/42"))
        );
        assert_eq!(ticket.out_name, "the tracker");
        let queued = context(vec![(ItemId(7), None, Place::Queue)]);
        let ticket = TicketView::of(ItemId(7), None, &queued);
        assert_eq!((ticket.main, ticket.out), (inside("/ui/queue/7"), None));
    }

    #[test]
    fn an_unplaced_ticket_leads_to_the_tracker_or_else_to_the_search() {
        let none = context(Vec::new());
        let gh = external("gh:acme/widgets#42");
        let tracked = TicketView::of(ItemId(7), Some(&gh), &none);
        assert_eq!(
            (tracked.main, tracked.out),
            (outside("https://github.com/acme/widgets/issues/42"), None)
        );
        let unknown = external("jira:ABC-12");
        let pageless = TicketView::of(ItemId(7), Some(&unknown), &none);
        assert_eq!(
            (pageless.main, pageless.out),
            (inside("/ui/search?q=jira%3AABC-12"), None)
        );
        let bare = TicketView::of(ItemId(7), None, &none);
        assert_eq!(bare.main, inside("/ui/search?q=%237"));
    }

    #[test]
    fn the_first_place_given_for_an_item_stands() {
        let both = context(vec![
            (ItemId(7), None, Place::Claims),
            (ItemId(7), None, Place::Queue),
        ]);
        assert_eq!(
            TicketView::of(ItemId(7), None, &both).main,
            inside("/ui/claims/7")
        );
    }

    #[test]
    fn an_item_named_by_id_is_named_as_the_context_knows_it_or_by_its_id() {
        let gh = external("gh:acme/widgets#42");
        let known = context(vec![(ItemId(7), Some(gh), Place::Claims)]);
        let ticket = TicketView::of_item(ItemId(7), &known);
        assert_eq!(ticket.label, "widgets#42");
        assert_eq!(ticket.main, inside("/ui/claims/7"));
        let gone = TicketView::of_item(ItemId(9), &known);
        assert_eq!(gone.label, "#9");
        assert_eq!((gone.main, gone.out), (inside("/ui/search?q=%239"), None));
    }

    #[test]
    fn only_a_web_address_becomes_an_outside_link() {
        for address in [
            "javascript:alert(1)",
            "JavaScript:alert(1)",
            "data:text/html,<b>x</b>",
            "//example.com/a",
            "/ui/claims",
            "ftp://example.com/a",
            "http:/example.com",
        ] {
            assert_eq!(Link::outside(address.to_owned()), None, "{address}");
        }
        for address in ["https://example.com/a", "HTTP://example.com/a"] {
            assert_eq!(
                Link::outside(address.to_owned()),
                Some(outside(address)),
                "{address}"
            );
        }
    }

    #[test]
    fn a_schemes_page_that_is_no_web_address_is_no_link() {
        let none = context(Vec::new());
        let hostile = external("evil:x-1");
        let ticket = TicketView::of(ItemId(3), Some(&hostile), &none);
        assert_eq!(
            (ticket.main, ticket.out),
            (inside("/ui/search?q=evil%3Ax-1"), None)
        );
        let placed = context(vec![(ItemId(3), Some(hostile.clone()), Place::Claims)]);
        assert_eq!(TicketView::of(ItemId(3), Some(&hostile), &placed).out, None);
        let text = LinkedText::of("see evil:x-1", None, &none);
        assert_eq!(
            text.pieces[1].link,
            Some(inside("/ui/search?q=evil%3Ax-1")),
            "{text:?}"
        );
    }

    #[test]
    fn initials_are_the_first_letter_and_the_one_after_a_separator_or_else_the_second() {
        let of = |handle: &str| PersonView::of(&Handle::parse(handle).unwrap()).initials;
        assert_eq!(of("alice"), "AL");
        assert_eq!(of("bob-smith"), "BS");
        assert_eq!(of("x"), "X");
        assert_eq!(initials("a-"), "A");
        assert_eq!(initials("carol_jones"), "CJ");
        assert_eq!(initials("d.e"), "DE");
    }

    #[test]
    fn a_persons_colour_and_address_depend_on_the_handle_alone() {
        for handle in ["alice", "bob", "carol", "bob-smith", "x"] {
            let person = PersonView::of(&Handle::parse(handle).unwrap());
            assert!((1..=6).contains(&person.colour), "{handle}");
            assert_eq!(
                person.colour,
                PersonView::of(&Handle::parse(handle).unwrap()).colour
            );
            assert_eq!(person.href, format!("/ui/claims?who={handle}"));
        }
        let colours: std::collections::BTreeSet<u8> = ["alice", "bob", "carol", "dave", "erin"]
            .iter()
            .map(|handle| PersonView::of(&Handle::parse(handle).unwrap()).colour)
            .collect();
        assert!(colours.len() > 1, "{colours:?}");
    }

    fn piece(text: &str, link: Option<Link>) -> LinkedPiece {
        LinkedPiece {
            text: text.to_owned(),
            link,
        }
    }

    #[test]
    fn a_text_links_each_reference_and_keeps_the_rest_as_written() {
        let gh = external("gh:acme/widgets#42");
        let placed = context(vec![(ItemId(5), Some(gh), Place::Queue)]);
        let text = "<script>x</script> gh:acme/widgets#42, acme/widgets#7 #8 in api:billing \
                    at 1a2b3c4d, see https://example.com/a.";
        let api = RepoId::parse("api").unwrap();
        let linked = LinkedText::of(text, Some(&api), &placed);
        assert_eq!(
            linked.pieces,
            vec![
                piece("<script>x</script> ", None),
                piece("gh:acme/widgets#42", Some(inside("/ui/queue/5"))),
                piece(", ", None),
                piece(
                    "acme/widgets#7",
                    Some(outside("https://github.com/acme/widgets/issues/7"))
                ),
                piece(" ", None),
                piece(
                    "#8",
                    Some(outside("https://github.com/acme/widgets/issues/8"))
                ),
                piece(" in ", None),
                piece("api:billing", Some(inside("/ui/areas/api:billing"))),
                piece(" at ", None),
                piece(
                    "1a2b3c4d",
                    Some(outside("https://github.com/acme/widgets/commit/1a2b3c4d"))
                ),
                piece(", see ", None),
                piece(
                    "https://example.com/a",
                    Some(outside("https://example.com/a"))
                ),
                piece(".", None),
            ]
        );
        let joined: String = linked
            .pieces
            .iter()
            .map(|piece| piece.text.as_str())
            .collect();
        assert_eq!(joined, text);
    }

    fn area(text: &str) -> QualifiedArea {
        QualifiedArea::parse(text).unwrap()
    }

    fn pr(text: &str) -> PrRef {
        PrRef::parse(text).unwrap()
    }

    fn remote(text: &str) -> RemoteUrl {
        RemoteUrl::parse(text).unwrap()
    }

    fn branch(name: &str) -> BranchName {
        BranchName::parse(name).unwrap()
    }

    #[test]
    fn an_area_is_named_short_in_a_row_and_whole_alone_and_leads_to_its_page() {
        let view = AreaView::of(&area("api:billing"));
        assert_eq!(
            view,
            AreaView {
                label: "billing".to_owned(),
                full: "api:billing".to_owned(),
                href: "/ui/areas/api:billing".to_owned(),
            }
        );
    }

    #[test]
    fn a_pull_request_of_a_project_repository_leads_to_the_forge_or_here_with_the_forge_beside() {
        let none = context(Vec::new());
        let twelve = pr("Acme/Widgets#12");
        let view = PullRequestView::of(&twelve, None, None, &none);
        assert_eq!(view.label, "widgets#12");
        assert_eq!(view.title, "acme/widgets#12");
        assert_eq!(
            (view.main, view.out),
            (outside("https://github.com/acme/widgets/pull/12"), None)
        );
        let here = PullRequestView::of(&twelve, Some("/ui/claims/3".to_owned()), None, &none);
        assert_eq!(here.main, inside("/ui/claims/3"));
        assert_eq!(
            here.out,
            Some(Beside {
                link: outside("https://github.com/acme/widgets/pull/12"),
                name: "GitHub",
            })
        );
    }

    #[test]
    fn a_pull_request_of_no_listed_remote_leads_through_its_items_repository_when_the_paths_agree()
    {
        let none = context(Vec::new());
        let gadgets = pr("acme/gadgets#5");
        let theirs = remote("https://github.com/acme/gadgets");
        assert_eq!(
            PullRequestView::of(&gadgets, None, Some(&theirs), &none).main,
            outside("https://github.com/acme/gadgets/pull/5")
        );
        let elsewhere = remote("https://github.com/acme/other");
        assert_eq!(
            PullRequestView::of(&gadgets, None, Some(&elsewhere), &none).main,
            inside("/ui/search?q=acme%2Fgadgets%235")
        );
        let here = PullRequestView::of(&gadgets, Some("/ui/claims/3".to_owned()), None, &none);
        assert_eq!((here.main, here.out), (inside("/ui/claims/3"), None));
    }

    #[test]
    fn a_pull_request_of_a_listed_remote_on_an_unknown_forge_leads_to_the_search() {
        let none = context(Vec::new());
        let docs = pr("acme/docs#4");
        let theirs = remote("https://github.com/acme/docs");
        let view = PullRequestView::of(&docs, None, Some(&theirs), &none);
        assert_eq!(
            (view.label.as_str(), view.main, view.out),
            ("docs#4", inside("/ui/search?q=acme%2Fdocs%234"), None)
        );
    }

    #[test]
    fn a_branch_leads_to_the_forges_page_else_to_the_claims_on_it() {
        let none = context(Vec::new());
        let api = RepoId::parse("api").unwrap();
        let docs = RepoId::parse("docs").unwrap();
        let fix = branch("alice/42-fix+now");
        let view = BranchView::of(&api, &fix, &none);
        assert_eq!(view.label, "alice/42-fix+now");
        assert_eq!(
            view.main,
            outside("https://github.com/acme/widgets/tree/alice/42-fix%2Bnow")
        );
        assert_eq!(
            BranchView::of(&docs, &fix, &none).main,
            inside("/ui/claims?q=alice%2F42-fix%2Bnow")
        );
    }

    #[test]
    fn a_file_leads_to_the_forges_page_at_its_revision_else_to_the_search() {
        let none = context(Vec::new());
        let file = RepoPath::parse("api:src/billing/a b.rs").unwrap();
        let fix = branch("alice/42-fix");
        let at_branch = FileView::of(&file, Revision::Branch(&fix), &none);
        assert_eq!(at_branch.label, "src/billing/a b.rs");
        assert_eq!(at_branch.title, "api:src/billing/a b.rs");
        assert_eq!(
            at_branch.main,
            outside("https://github.com/acme/widgets/blob/alice/42-fix/src/billing/a%20b.rs")
        );
        assert_eq!(
            FileView::of(&file, Revision::DefaultBranch, &none).main,
            outside("https://github.com/acme/widgets/blob/HEAD/src/billing/a%20b.rs")
        );
        let guide = RepoPath::parse("docs:guide/intro.md").unwrap();
        assert_eq!(
            FileView::of(&guide, Revision::DefaultBranch, &none).main,
            inside("/ui/search?q=docs%3Aguide%2Fintro.md")
        );
    }

    /// A forge whose pages are at addresses that are not web addresses.
    #[derive(Debug)]
    struct HostileForge;

    impl Forge for HostileForge {
        fn name(&self) -> &'static str {
            "Hostile"
        }

        fn issue(&self, _: &RemoteUrl, _: u64) -> String {
            "javascript:alert(1)".to_owned()
        }

        fn pull_request(&self, _: &RemoteUrl, _: u64) -> String {
            "javascript:alert(1)".to_owned()
        }

        fn branch(&self, _: &RemoteUrl, _: &BranchName) -> String {
            "javascript:alert(1)".to_owned()
        }

        fn file(&self, _: &RemoteUrl, _: Revision<'_>, _: &str) -> String {
            "javascript:alert(1)".to_owned()
        }

        fn directory(&self, _: &RemoteUrl, _: Revision<'_>, _: &str) -> String {
            "javascript:alert(1)".to_owned()
        }

        fn commit(&self, _: &RemoteUrl, _: &str) -> String {
            "javascript:alert(1)".to_owned()
        }
    }

    #[test]
    fn a_directory_leads_to_the_forges_page_at_the_default_branch() {
        let none = context(Vec::new());
        let api = RepoId::parse("api").unwrap();
        let view = DirectoryView::of(&api, "src/billing", &none);
        assert_eq!(view.label, "src/billing");
        assert_eq!(view.title, "api:src/billing");
        assert_eq!(
            view.main,
            outside("https://github.com/acme/widgets/tree/HEAD/src/billing")
        );
    }

    #[test]
    fn the_root_directory_is_named_by_a_slash_and_leads_to_the_repositorys_tree() {
        let none = context(Vec::new());
        let api = RepoId::parse("api").unwrap();
        let view = DirectoryView::of(&api, "", &none);
        assert_eq!((view.label.as_str(), view.title.as_str()), ("/", "api:/"));
        assert_eq!(
            view.main,
            outside("https://github.com/acme/widgets/tree/HEAD")
        );
    }

    #[test]
    fn a_directory_of_a_repository_with_no_remote_or_no_known_forge_leads_to_the_search() {
        let document: SettingsDocument = serde_json::from_value(serde_json::json!({
            "areas": [{"name": "billing", "paths": ["src/billing/**"]}]
        }))
        .unwrap();
        let local = LinkContext::new(
            ProjectSettings::parse(&document, &HotRuleRegistry::standard()).unwrap(),
            Arc::new(RefSchemeRegistry::standard()),
            Arc::new(ForgeRegistry::standard()),
            Vec::new(),
        );
        let view = DirectoryView::of(&RepoId::implicit(), "src/billing", &local);
        assert_eq!(view.main, inside("/ui/search?q=default%3Asrc%2Fbilling"));
        let none = context(Vec::new());
        let docs = RepoId::parse("docs").unwrap();
        assert_eq!(
            DirectoryView::of(&docs, "guide", &none).main,
            inside("/ui/search?q=docs%3Aguide")
        );
        assert_eq!(
            DirectoryView::of(&docs, "", &none).main,
            inside("/ui/search?q=docs%3A%2F")
        );
    }

    #[test]
    fn a_directory_page_that_is_no_web_address_is_no_link() {
        let hostile = LinkContext::new(
            settings(),
            Arc::new(RefSchemeRegistry::standard()),
            Arc::new(ForgeRegistry::standard().with("github.com", Box::new(HostileForge))),
            Vec::new(),
        );
        let api = RepoId::parse("api").unwrap();
        assert_eq!(
            DirectoryView::of(&api, "src/billing", &hostile).main,
            inside("/ui/search?q=api%3Asrc%2Fbilling")
        );
    }

    #[test]
    fn a_conversation_leads_to_its_page_and_links_the_references_in_its_subject() {
        let none = context(Vec::new());
        let view = ConversationView::of(ThreadId(9), "About api:billing", &none);
        assert_eq!(view.href, "/ui/inbox/9");
        assert_eq!(
            view.subject.pieces,
            vec![
                piece("About ", None),
                piece("api:billing", Some(inside("/ui/areas/api:billing"))),
            ]
        );
    }

    #[test]
    fn a_numbered_reference_of_a_repository_leads_to_the_forges_issue_else_to_the_search() {
        let none = context(Vec::new());
        let api = RepoId::parse("api").unwrap();
        let docs = RepoId::parse("docs").unwrap();
        assert_eq!(
            none.numbered(&api, 77),
            outside("https://github.com/acme/widgets/issues/77")
        );
        assert_eq!(none.numbered(&docs, 77), inside("/ui/search?q=%2377"));
    }

    #[test]
    fn a_reference_no_address_can_be_built_for_stays_plain_and_joins_its_neighbours() {
        let none = context(Vec::new());
        let docs = RepoId::parse("docs").unwrap();
        let linked = LinkedText::of("fixes #8 in docs:guide", Some(&docs), &none);
        assert_eq!(
            linked.pieces,
            vec![
                piece("fixes #8 in ", None),
                piece("docs:guide", Some(inside("/ui/areas/docs:guide"))),
            ]
        );
    }
}

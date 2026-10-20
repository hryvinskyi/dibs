//! Conversations: subjects, bodies, recipients, who may read a thread, how much a message asks
//! for its readers' attention, and the acknowledgements a message may ask of the people it reaches.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::{Duration, OffsetDateTime};

use crate::ids::{MemberId, MessageId};
use crate::member::Role;
use crate::text::{has_control, string_value};

/// A thread's subject: 1 to 200 characters after trimming, with no control characters.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Subject(String);

/// Why a text is not a subject.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SubjectError {
    /// The trimmed text is empty, longer than 200 characters, or has control characters.
    #[error("a subject is 1 to 200 characters with no control characters")]
    Malformed,
}

impl Subject {
    /// Parses a subject, trimming whitespace at both ends.
    ///
    /// # Errors
    /// [`SubjectError::Malformed`] when the trimmed text breaks the rules above.
    pub fn parse(value: &str) -> Result<Self, SubjectError> {
        let value = value.trim();
        if value.is_empty() || value.chars().count() > 200 || has_control(value) {
            return Err(SubjectError::Malformed);
        }
        Ok(Self(value.to_owned()))
    }
}

string_value!(Subject, SubjectError);

/// A message's body: plain text of 1 to 16384 bytes with no NUL, not only whitespace. Line breaks
/// and other whitespace are kept as written.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Body(String);

/// Why a text is not a body.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BodyError {
    /// The text is empty or only whitespace.
    #[error("a message is empty")]
    Empty,
    /// The text is longer than 16384 bytes.
    #[error("a message is longer than 16384 bytes")]
    TooLong,
    /// The text contains a NUL character.
    #[error("a message contains a NUL character")]
    Nul,
}

impl Body {
    /// The largest body, in bytes.
    pub const MAX_BYTES: usize = 16_384;

    /// Parses a body.
    ///
    /// # Errors
    /// [`BodyError`] naming the rule broken.
    pub fn parse(value: &str) -> Result<Self, BodyError> {
        if value.trim().is_empty() {
            return Err(BodyError::Empty);
        }
        if value.len() > Self::MAX_BYTES {
            return Err(BodyError::TooLong);
        }
        if value.contains('\0') {
            return Err(BodyError::Nul);
        }
        Ok(Self(value.to_owned()))
    }
}

string_value!(Body, BodyError);

/// The label of the session that wrote a message (`laptop/4f2a91c0`, `web`): 1 to 64 characters
/// after trimming, with no control characters.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct SessionLabel(String);

/// Why a text is not a session label.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SessionLabelError {
    /// The trimmed text is empty, longer than 64 characters, or has control characters.
    #[error("a session label is 1 to 64 characters with no control characters")]
    Malformed,
}

impl SessionLabel {
    /// Parses a label, trimming whitespace at both ends.
    ///
    /// # Errors
    /// [`SessionLabelError::Malformed`] when the trimmed text breaks the rules above.
    pub fn parse(value: &str) -> Result<Self, SessionLabelError> {
        let value = value.trim();
        if value.is_empty() || value.chars().count() > 64 || has_control(value) {
            return Err(SessionLabelError::Malformed);
        }
        Ok(Self(value.to_owned()))
    }
}

string_value!(SessionLabel, SessionLabelError);

/// Whom a thread is addressed to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recipients {
    /// Every person in the project.
    Project,
    /// These members; built by [`Recipients::members`], so never empty.
    Members(BTreeSet<MemberId>),
}

/// Why a recipient list is refused.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RecipientsError {
    /// No member was named.
    #[error("a message names at least one recipient")]
    Empty,
}

impl Recipients {
    /// Addresses these members.
    ///
    /// # Errors
    /// [`RecipientsError::Empty`] when no member is named.
    pub fn members(members: BTreeSet<MemberId>) -> Result<Self, RecipientsError> {
        if members.is_empty() {
            return Err(RecipientsError::Empty);
        }
        Ok(Self::Members(members))
    }
}

/// Whether `reader` may read a thread `author` started with these recipients: its author, its
/// recipients, and for a project-addressed thread every person. An integration bot reads no thread.
pub fn may_read(author: MemberId, recipients: &Recipients, reader: MemberId, role: Role) -> bool {
    if !role.is_person() {
        return false;
    }
    if reader == author {
        return true;
    }
    match recipients {
        Recipients::Project => true,
        Recipients::Members(members) => members.contains(&reader),
    }
}

/// How much a message asks for the attention of the people it reaches. Ordered from the least to
/// the most pressing.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Importance {
    /// An ordinary message: read when convenient.
    #[default]
    Normal,
    /// A message to read soon.
    High,
    /// A message to read now.
    Urgent,
}

/// Why a text is not an importance.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ImportanceError {
    /// The text is none of the names.
    #[error("an importance is normal, high or urgent")]
    Unknown,
}

impl Importance {
    /// Every importance, from the least to the most pressing.
    pub const ALL: [Self; 3] = [Self::Normal, Self::High, Self::Urgent];

    /// The stable name: `normal`, `high` or `urgent`.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::High => "high",
            Self::Urgent => "urgent",
        }
    }

    /// The importance of this name.
    ///
    /// # Errors
    /// [`ImportanceError::Unknown`] for any other text.
    pub fn parse(text: &str) -> Result<Self, ImportanceError> {
        Self::ALL
            .into_iter()
            .find(|importance| importance.name() == text)
            .ok_or(ImportanceError::Unknown)
    }

    /// Whether it is raised above an ordinary message: `high` or `urgent`.
    pub const fn is_raised(self) -> bool {
        matches!(self, Self::High | Self::Urgent)
    }
}

impl fmt::Display for Importance {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Whether a message should reach its recipients while they work rather than wait for them to
/// look: a raised importance, or an acknowledgement asked of them.
pub const fn interrupts(importance: Importance, ack_required: bool) -> bool {
    importance.is_raised() || ack_required
}

/// Who wrote a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Writer {
    /// A member, through the message routes: what they or their agent typed, at the importance
    /// they chose.
    Member,
    /// dibs itself: an announcement, a notice or a yield request it sends when something happens,
    /// in the project's name or in the name of the member whose action caused it.
    Dibs,
}

/// How far back the limit on raised messages looks.
pub const RAISED_WINDOW: Duration = Duration::hours(1);

/// Whether a message counts toward its writer's limit on raised messages: one a member wrote at a
/// raised importance. A message dibs writes never counts, so the announcements a wide claim over
/// many held areas sends never refuse the claim. This is the one place that says so.
pub const fn counts_toward_limit(writer: Writer, importance: Importance) -> bool {
    matches!(writer, Writer::Member) && importance.is_raised()
}

/// That a member wrote as many raised messages within [`RAISED_WINDOW`] as the project allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error(
    "you sent {limit} high or urgent messages within the last hour, as many as this project allows"
)]
pub struct ImportanceLimited {
    /// How many the project allows within the window.
    pub limit: u32,
    /// When the oldest of them leaves the window, so that one more may be sent.
    pub frees_at: OffsetDateTime,
}

/// Decides whether a message of `importance` by `writer` may be written at `now`, given when the
/// writer's counted messages were written (in any order; those older than [`RAISED_WINDOW`] are
/// ignored) and the project's `limit`. Answers whether the message counts, so it is recorded with
/// the others.
///
/// # Errors
/// [`ImportanceLimited`] when the writer already wrote `limit` counted messages within the window,
/// with when the next one is allowed.
pub fn decide_raised(
    writer: Writer,
    importance: Importance,
    written: &[OffsetDateTime],
    limit: u32,
    now: OffsetDateTime,
) -> Result<bool, ImportanceLimited> {
    if !counts_toward_limit(writer, importance) {
        return Ok(false);
    }
    let mut recent: Vec<OffsetDateTime> = written
        .iter()
        .copied()
        .filter(|at| *at <= now && now - *at < RAISED_WINDOW)
        .collect();
    recent.sort_unstable();
    let allowed = usize::try_from(limit).unwrap_or(usize::MAX);
    if recent.len() < allowed {
        return Ok(true);
    }
    let oldest_counting = recent.len() - allowed;
    let frees_at = recent
        .get(oldest_counting)
        .map_or(now, |at| *at + RAISED_WINDOW);
    Err(ImportanceLimited { limit, frees_at })
}

/// A member of a project as the recipients of a message are told from: their role, and when they
/// joined.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Person {
    /// The member.
    pub member: MemberId,
    /// Their role; only people receive messages.
    pub role: Role,
    /// When they joined the project.
    pub joined_at: OffsetDateTime,
}

/// The members a message reaches: everyone who may read its thread but its own author — the
/// thread's author and the members it names, or for a thread addressed to the project every person
/// who was in the project when the message was written. Of the members named, only the people
/// among `members` count, so a member who is gone or an integration is never a recipient.
pub fn message_recipients(
    thread_author: MemberId,
    recipients: &Recipients,
    message_author: MemberId,
    written_at: OffsetDateTime,
    members: &[Person],
) -> BTreeSet<MemberId> {
    let people = members.iter().filter(|person| person.role.is_person());
    let reached: BTreeSet<MemberId> = match recipients {
        Recipients::Project => people
            .filter(|person| person.joined_at <= written_at)
            .map(|person| person.member)
            .collect(),
        Recipients::Members(named) => people
            .map(|person| person.member)
            .filter(|member| *member == thread_author || named.contains(member))
            .collect(),
    };
    reached
        .into_iter()
        .filter(|member| *member != message_author)
        .collect()
}

/// That a recipient acknowledged a message: their own record, kept beside the message, which never
/// changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ack {
    /// The message.
    pub message: MessageId,
    /// The recipient who acknowledged it.
    pub member: MemberId,
    /// When.
    pub at: OffsetDateTime,
}

/// Why an acknowledgement is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum AckRefused {
    /// The message does not ask to be acknowledged.
    #[error("this message does not ask to be acknowledged")]
    NotRequired,
    /// The member is not one of the message's recipients; its author is not either.
    #[error("only a member this message was sent to can acknowledge it")]
    NotRecipient,
}

/// What acknowledging a message comes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AckOutcome {
    /// A new acknowledgement, to be recorded.
    New(Ack),
    /// The member acknowledged the message before; their first acknowledgement stands and nothing
    /// is recorded again.
    Already(Ack),
}

impl AckOutcome {
    /// The acknowledgement that stands.
    pub const fn ack(self) -> Ack {
        match self {
            Self::New(ack) | Self::Already(ack) => ack,
        }
    }
}

/// Where a message's acknowledgements stand: for each of its recipients, when they acknowledged it,
/// or that they have not yet. Derived from the acknowledgements recorded, never kept on the
/// message; an acknowledgement by a member who is not a recipient counts for nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AckState {
    recipients: BTreeMap<MemberId, Option<OffsetDateTime>>,
}

impl AckState {
    /// The state of a message reaching `recipients`, given the acknowledgements recorded for it.
    pub fn derive(recipients: &BTreeSet<MemberId>, acks: &[Ack]) -> Self {
        let mut state: BTreeMap<MemberId, Option<OffsetDateTime>> =
            recipients.iter().map(|member| (*member, None)).collect();
        for ack in acks {
            if let Some(slot) = state.get_mut(&ack.member) {
                *slot = Some(slot.map_or(ack.at, |earlier| earlier.min(ack.at)));
            }
        }
        Self { recipients: state }
    }

    /// Whether `member` is a recipient, and if so when they acknowledged: `None` for somebody who
    /// is not a recipient, `Some(None)` for a recipient who has not acknowledged yet.
    pub fn of(&self, member: MemberId) -> Option<Option<OffsetDateTime>> {
        self.recipients.get(&member).copied()
    }

    /// Whether `member` is a recipient who has not acknowledged yet.
    pub fn awaits(&self, member: MemberId) -> bool {
        self.of(member) == Some(None)
    }

    /// Every recipient with when they acknowledged, in member order.
    pub fn entries(&self) -> impl Iterator<Item = (MemberId, Option<OffsetDateTime>)> + '_ {
        self.recipients.iter().map(|(member, at)| (*member, *at))
    }

    /// Whether every recipient has acknowledged.
    pub fn is_complete(&self) -> bool {
        self.recipients.values().all(Option::is_some)
    }
}

/// Decides `member`'s acknowledgement of `message` at `now`: only a message that asks for one, and
/// only by one of its recipients. A recipient acknowledges once: asking again answers the
/// acknowledgement they gave, with its first time, so a retry never meets a refusal.
///
/// # Errors
/// [`AckRefused`] naming the rule broken.
pub fn decide_ack(
    message: MessageId,
    ack_required: bool,
    state: &AckState,
    member: MemberId,
    now: OffsetDateTime,
) -> Result<AckOutcome, AckRefused> {
    if !ack_required {
        return Err(AckRefused::NotRequired);
    }
    let ack = |at| Ack {
        message,
        member,
        at,
    };
    match state.of(member) {
        None => Err(AckRefused::NotRecipient),
        Some(Some(at)) => Ok(AckOutcome::Already(ack(at))),
        Some(None) => Ok(AckOutcome::New(ack(now))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{ALICE, BOB, CAROL, at};

    const DAVE: MemberId = MemberId(4);
    const BOT: MemberId = MemberId(9);

    fn people() -> Vec<Person> {
        let person = |member, role, minutes| Person {
            member,
            role,
            joined_at: at(minutes),
        };
        vec![
            person(ALICE, Role::Admin, 0),
            person(BOB, Role::Member, 0),
            person(CAROL, Role::Member, 5),
            person(DAVE, Role::Member, 20),
            person(BOT, Role::Importer, 0),
        ]
    }

    #[test]
    fn subject_table() {
        assert_eq!(
            Subject::parse("  Who owns billing?  ").unwrap().as_str(),
            "Who owns billing?"
        );
        assert!(Subject::parse(&"s".repeat(200)).is_ok());
        for bad in ["", "   ", "two\nlines", &"s".repeat(201)] {
            assert_eq!(Subject::parse(bad), Err(SubjectError::Malformed), "{bad:?}");
        }
    }

    #[test]
    fn body_table() {
        let kept = "first line\n\n  indented\tand tabbed\n";
        assert_eq!(Body::parse(kept).unwrap().as_str(), kept);
        let cyrillic = "ж".repeat(Body::MAX_BYTES / 2);
        assert!(
            Body::parse(&cyrillic).is_ok(),
            "16 KiB of two-byte characters fit"
        );
        assert_eq!(
            Body::parse(&format!("{cyrillic}x")),
            Err(BodyError::TooLong)
        );
        assert_eq!(Body::parse(""), Err(BodyError::Empty));
        assert_eq!(Body::parse(" \n\t"), Err(BodyError::Empty));
        assert_eq!(Body::parse("a\u{0}b"), Err(BodyError::Nul));
    }

    #[test]
    fn session_label_table() {
        assert_eq!(
            SessionLabel::parse("laptop/4f2a91c0").unwrap().as_str(),
            "laptop/4f2a91c0"
        );
        assert!(SessionLabel::parse("web").is_ok());
        assert!(SessionLabel::parse(&"l".repeat(64)).is_ok());
        for bad in ["", "  ", "a\tb", &"l".repeat(65)] {
            assert_eq!(
                SessionLabel::parse(bad),
                Err(SessionLabelError::Malformed),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn recipients_name_someone() {
        assert_eq!(
            Recipients::members(BTreeSet::new()),
            Err(RecipientsError::Empty)
        );
        assert!(Recipients::members(BTreeSet::from([BOB])).is_ok());
    }

    #[test]
    fn who_may_read_a_thread() {
        let to_bob = Recipients::members(BTreeSet::from([BOB])).unwrap();
        assert!(may_read(ALICE, &to_bob, ALICE, Role::Member), "the author");
        assert!(may_read(ALICE, &to_bob, BOB, Role::Member), "a recipient");
        assert!(
            !may_read(ALICE, &to_bob, CAROL, Role::Member),
            "anyone else"
        );
        assert!(
            !may_read(ALICE, &to_bob, CAROL, Role::Admin),
            "an admin is not a recipient"
        );
        assert!(
            may_read(ALICE, &Recipients::Project, CAROL, Role::Member),
            "the project"
        );
        assert!(
            !may_read(ALICE, &Recipients::Project, CAROL, Role::Importer),
            "an integration reads no thread"
        );
    }

    #[test]
    fn importance_names_order_and_raise() {
        for importance in Importance::ALL {
            assert_eq!(Importance::parse(importance.name()), Ok(importance));
            assert_eq!(importance.to_string(), importance.name());
            let json = serde_json::to_value(importance).unwrap();
            assert_eq!(json, importance.name());
        }
        assert_eq!(Importance::default(), Importance::Normal);
        assert!(Importance::Normal < Importance::High && Importance::High < Importance::Urgent);
        assert_eq!(Importance::parse("URGENT"), Err(ImportanceError::Unknown));
        assert_eq!(Importance::parse(""), Err(ImportanceError::Unknown));
        assert!(!Importance::Normal.is_raised());
        assert!(Importance::High.is_raised() && Importance::Urgent.is_raised());
    }

    #[test]
    fn a_message_interrupts_when_raised_or_when_it_asks_for_an_ack() {
        assert!(!interrupts(Importance::Normal, false));
        assert!(interrupts(Importance::Normal, true));
        assert!(interrupts(Importance::High, false));
        assert!(interrupts(Importance::Urgent, false));
    }

    #[test]
    fn a_message_reaches_its_thread_s_readers_but_its_author() {
        let to_bob = Recipients::members(BTreeSet::from([BOB, BOT])).unwrap();
        assert_eq!(
            message_recipients(ALICE, &to_bob, ALICE, at(30), &people()),
            BTreeSet::from([BOB]),
            "the members named, never an integration or the author"
        );
        assert_eq!(
            message_recipients(ALICE, &to_bob, BOB, at(30), &people()),
            BTreeSet::from([ALICE]),
            "a reply reaches the thread's author"
        );
        assert_eq!(
            message_recipients(ALICE, &Recipients::Project, BOB, at(10), &people()),
            BTreeSet::from([ALICE, CAROL]),
            "the people in the project when it was written"
        );
        assert_eq!(
            message_recipients(ALICE, &Recipients::Project, BOB, at(20), &people()),
            BTreeSet::from([ALICE, CAROL, DAVE]),
            "a member who joined at that moment is reached"
        );
        let gone = Recipients::members(BTreeSet::from([MemberId(77)])).unwrap();
        assert!(
            message_recipients(ALICE, &gone, ALICE, at(30), &people()).is_empty(),
            "a member who is gone is no recipient"
        );
    }

    fn ack(member: MemberId, minutes: i64) -> Ack {
        Ack {
            message: MessageId(7),
            member,
            at: at(minutes),
        }
    }

    #[test]
    fn the_state_of_acks_is_derived_per_recipient() {
        let recipients = BTreeSet::from([BOB, CAROL]);
        let state = AckState::derive(&recipients, &[ack(CAROL, 3), ack(DAVE, 1), ack(CAROL, 2)]);
        assert_eq!(
            state.entries().collect::<Vec<_>>(),
            [(BOB, None), (CAROL, Some(at(2)))],
            "an outsider's ack counts for nothing; the first ack stands"
        );
        assert!(state.awaits(BOB));
        assert!(!state.awaits(CAROL));
        assert!(!state.awaits(DAVE));
        assert_eq!(state.of(DAVE), None);
        assert!(!state.is_complete());
        let done = AckState::derive(&recipients, &[ack(BOB, 1), ack(CAROL, 2)]);
        assert!(done.is_complete());
    }

    #[test]
    fn only_a_recipient_acks_and_once_a_repeat_answers_the_first() {
        let recipients = BTreeSet::from([BOB, CAROL]);
        let state = AckState::derive(&recipients, &[ack(CAROL, 2)]);
        assert_eq!(
            decide_ack(MessageId(7), true, &state, BOB, at(9)),
            Ok(AckOutcome::New(ack(BOB, 9)))
        );
        let repeat = decide_ack(MessageId(7), true, &state, CAROL, at(9));
        assert_eq!(
            repeat,
            Ok(AckOutcome::Already(ack(CAROL, 2))),
            "the first acknowledgement stands, with its time"
        );
        assert_eq!(repeat.map(AckOutcome::ack), Ok(ack(CAROL, 2)));
        assert_eq!(
            decide_ack(MessageId(7), true, &state, ALICE, at(9)),
            Err(AckRefused::NotRecipient),
            "the author is not a recipient"
        );
        assert_eq!(
            decide_ack(MessageId(7), false, &state, BOB, at(9)),
            Err(AckRefused::NotRequired)
        );
    }

    #[test]
    fn a_members_eleventh_raised_message_within_the_hour_is_refused_until_the_oldest_ages_out() {
        let start = OffsetDateTime::UNIX_EPOCH + Duration::days(1);
        let written: Vec<OffsetDateTime> =
            (0..10).map(|n| start + Duration::minutes(n * 5)).collect();
        let now = start + Duration::minutes(50);
        assert_eq!(
            decide_raised(Writer::Member, Importance::Urgent, &written[..9], 10, now),
            Ok(true)
        );
        assert_eq!(
            decide_raised(Writer::Member, Importance::High, &written, 10, now),
            Err(ImportanceLimited {
                limit: 10,
                frees_at: start + Duration::hours(1)
            })
        );
        assert_eq!(
            decide_raised(
                Writer::Member,
                Importance::High,
                &written,
                10,
                start + Duration::hours(1)
            ),
            Ok(true),
            "the oldest left the window"
        );
        assert_eq!(
            decide_raised(Writer::Member, Importance::Normal, &written, 10, now),
            Ok(false),
            "an ordinary message is never limited"
        );
    }

    #[test]
    fn what_dibs_writes_never_counts_or_is_refused() {
        let now = OffsetDateTime::UNIX_EPOCH + Duration::days(1);
        let written = vec![now; 50];
        assert!(!counts_toward_limit(Writer::Dibs, Importance::Urgent));
        assert_eq!(
            decide_raised(Writer::Dibs, Importance::Urgent, &written, 10, now),
            Ok(false)
        );
    }
}

//! What each tool takes. The structs are the tools' input schemas; the checks turn their plain
//! values into the domain types the client sends, naming the field at fault in a sentence the agent
//! can act on. Nothing here talks to the server.

use std::path::PathBuf;

use rmcp::schemars::{self, JsonSchema};
use serde::Deserialize;

use dibs_client::api::{GrantRequest, ItemFilter, NewItem, Recipients, WideRequest};
use dibs_domain::handle::Handle;
use dibs_domain::ids::{ItemId, MessageId, ThreadId};
use dibs_domain::item::{ExternalRef, ItemKey, ItemStatus, Title};
use dibs_domain::message::{Body, Importance, Subject};
use dibs_domain::slot::{Purpose, SlotName};
use dibs_domain::wide::WideReason;

use crate::failure::{Failure, sentence};

/// A work item: its local id or its reference.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum ItemKeyInput {
    /// The local id, as a number.
    Id(u64),
    /// The local id as text (`12`) or the reference in the outside tracker (`gh:acme/api#42`).
    Text(String),
}

impl ItemKeyInput {
    /// The key it names.
    ///
    /// # Errors
    /// A [`Failure`] naming `field` when it is neither a local id nor a reference.
    pub fn key(&self, field: &str) -> Result<ItemKey, Failure> {
        let text = match self {
            Self::Id(id) => id.to_string(),
            Self::Text(text) => text.trim().to_owned(),
        };
        ItemKey::parse(&text).map_err(|error| {
            invalid(
                field,
                &format!(
                    "'{text}' is not a work item ({error}); name it by its local id (12) or its reference (gh:acme/api#42)"
                ),
            )
        })
    }
}

/// A state of a work item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StatusInput {
    /// Nobody holds it.
    Open,
    /// A member holds it.
    Claimed,
    /// Completed.
    Done,
    /// Given up.
    Dropped,
}

impl From<StatusInput> for ItemStatus {
    fn from(value: StatusInput) -> Self {
        match value {
            StatusInput::Open => Self::Open,
            StatusInput::Claimed => Self::Claimed,
            StatusInput::Done => Self::Done,
            StatusInput::Dropped => Self::Dropped,
        }
    }
}

/// `dibs_items`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ItemsInput {
    /// Only the items in this state.
    pub status: Option<StatusInput>,
    /// Only the items you hold.
    pub mine: Option<bool>,
}

impl ItemsInput {
    /// The filter, `me` being the caller's handle.
    pub fn filter(&self, me: &Handle) -> ItemFilter {
        ItemFilter {
            status: self.status.map(Into::into),
            holder: self.mine.unwrap_or(false).then(|| me.clone()),
            external_ref: None,
        }
    }
}

/// `dibs_new`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NewInput {
    /// The reference in the outside tracker, `scheme:value` (`gh:acme/api#42`).
    #[serde(rename = "ref")]
    pub reference: String,
    /// The title, up to 200 characters.
    pub title: String,
}

impl NewInput {
    /// The item to create.
    ///
    /// # Errors
    /// A [`Failure`] naming the field at fault.
    pub fn item(&self) -> Result<NewItem, Failure> {
        let reference = ExternalRef::parse(self.reference.trim()).map_err(|error| {
            invalid(
                "ref",
                &format!("'{}' is not a reference ({error}); write it as scheme:value, such as gh:acme/api#42", self.reference),
            )
        })?;
        let title = Title::parse(&self.title)
            .map_err(|error| invalid("title", &format!("the title is not valid ({error})")))?;
        Ok(NewItem {
            external_ref: Some(reference),
            title,
        })
    }
}

/// `dibs_claim`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ClaimInput {
    /// The item: its local id or its reference.
    pub key: ItemKeyInput,
    /// The areas you will change: `repo:area`, or a name one repository declares. Leave it out to
    /// claim the item without areas.
    #[serde(default)]
    pub areas: Vec<String>,
    /// Make the claim wide, for a change that cuts across many areas: it holds none of them, and
    /// each member holding one is asked to acknowledge, which lets you edit there.
    #[serde(default)]
    pub wide: Option<WideClaimInput>,
}

/// The `wide` part of `dibs_claim`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WideClaimInput {
    /// The areas the change touches, named like `areas`.
    pub areas: Vec<String>,
    /// Why the change is made, in one or two sentences the holders read: up to 300 characters.
    pub reason: String,
}

impl ClaimInput {
    /// The wide part of the claim as the client sends it.
    ///
    /// # Errors
    /// A [`Failure`] when the reason is empty, too long or has control characters.
    pub fn wide(&self) -> Result<Option<WideRequest>, Failure> {
        self.wide
            .as_ref()
            .map(|wide| {
                WideReason::parse(&wide.reason).map_err(|error| {
                    invalid("wide.reason", &format!("the reason is not valid ({error})"))
                })?;
                Ok(WideRequest {
                    areas: wide.areas.clone(),
                    reason: wide.reason.clone(),
                })
            })
            .transpose()
    }
}

/// `dibs_takeover`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TakeoverInput {
    /// The item whose stale claim you take over: its local id or its reference.
    pub key: ItemKeyInput,
    /// Why you take it over; the member who held it is sent this.
    pub reason: String,
    /// The areas you will change, as for `dibs_claim`.
    #[serde(default)]
    pub areas: Vec<String>,
}

impl TakeoverInput {
    /// The reason, as the message the previous holder receives.
    ///
    /// # Errors
    /// A [`Failure`] when it is empty or not a valid message.
    pub fn reason(&self) -> Result<Body, Failure> {
        body("reason", &self.reason)
    }
}

/// `dibs_release` and `dibs_done`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct KeyInput {
    /// The item: its local id or its reference.
    pub key: ItemKeyInput,
}

/// `dibs_check`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CheckInput {
    /// The files you are about to change, relative to the working directory or absolute.
    pub paths: Vec<String>,
}

impl CheckInput {
    /// The paths as file-system paths.
    ///
    /// # Errors
    /// A [`Failure`] when there are none, or one is empty.
    pub fn paths(&self) -> Result<Vec<PathBuf>, Failure> {
        if self.paths.is_empty() {
            return Err(invalid("paths", "name at least one path"));
        }
        if self.paths.iter().any(|path| path.trim().is_empty()) {
            return Err(invalid("paths", "a path is empty"));
        }
        Ok(self.paths.iter().map(PathBuf::from).collect())
    }
}

/// `dibs_grant`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GrantInput {
    /// The work item the grantee edits for: its local id or its reference.
    pub key: ItemKeyInput,
    /// The area you hold in which they may edit.
    pub area: String,
    /// The handle of the member who may edit.
    pub to: String,
}

impl GrantInput {
    /// The grant, for the item `key` resolved to.
    ///
    /// # Errors
    /// A [`Failure`] when the handle is not valid.
    pub fn request(&self, item: ItemId) -> Result<GrantRequest, Failure> {
        Ok(GrantRequest {
            item,
            area: self.area.clone(),
            to: handle("to", &self.to)?,
        })
    }
}

/// How much a message asks for its recipients' attention.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ImportanceInput {
    /// An ordinary message: read when convenient.
    Normal,
    /// Read soon.
    High,
    /// Read now.
    Urgent,
}

impl From<ImportanceInput> for Importance {
    fn from(value: ImportanceInput) -> Self {
        match value {
            ImportanceInput::Normal => Self::Normal,
            ImportanceInput::High => Self::High,
            ImportanceInput::Urgent => Self::Urgent,
        }
    }
}

/// `dibs_send`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SendInput {
    /// The handles of the members to start a conversation with; or use `project`.
    pub to: Option<Vec<String>>,
    /// Start the conversation with every person in the project.
    pub project: Option<bool>,
    /// The subject of a new conversation.
    pub subject: Option<String>,
    /// The message.
    pub body: String,
    /// The work item a new conversation is about: its local id or its reference.
    pub item: Option<ItemKeyInput>,
    /// The id of an existing conversation to reply to; then give only this, the body and, if
    /// needed, importance and ack_required.
    pub thread: Option<u64>,
    /// How much the message asks for its recipients' attention: normal (the default), high or
    /// urgent.
    pub importance: Option<ImportanceInput>,
    /// Ask each recipient to acknowledge the message; false by default.
    pub ack_required: Option<bool>,
}

/// What a `dibs_send` does.
#[derive(Debug)]
pub enum Sending {
    /// A reply in an existing conversation.
    Reply {
        /// The conversation.
        thread: ThreadId,
        /// The message.
        body: Body,
        /// How much it asks for its recipients' attention.
        importance: Importance,
        /// Whether each recipient is asked to acknowledge it.
        ack_required: bool,
    },
    /// A new conversation.
    Start {
        /// Whom it goes to.
        to: Recipients,
        /// Its subject.
        subject: Subject,
        /// The work item it is about.
        item: Option<ItemKey>,
        /// The first message.
        body: Body,
        /// How much the first message asks for its recipients' attention.
        importance: Importance,
        /// Whether each recipient is asked to acknowledge the first message.
        ack_required: bool,
    },
}

impl SendInput {
    /// Decides what the input asks for.
    ///
    /// # Errors
    /// A [`Failure`] when the input mixes a reply with a new conversation, names no recipient or
    /// both kinds, or has a value that is not valid.
    pub fn sending(&self) -> Result<Sending, Failure> {
        let body = body("body", &self.body)?;
        let importance = self.importance.map_or(Importance::Normal, Importance::from);
        let ack_required = self.ack_required.unwrap_or(false);
        let project = self.project.unwrap_or(false);
        if let Some(thread) = self.thread {
            if self.to.is_some() || project || self.subject.is_some() || self.item.is_some() {
                return Err(invalid(
                    "thread",
                    "a reply takes only the thread, the body, importance and ack_required; leave out to, project, subject and item, or leave out thread to start a new conversation",
                ));
            }
            return Ok(Sending::Reply {
                thread: thread_id("thread", thread)?,
                body,
                importance,
                ack_required,
            });
        }
        let to = match (&self.to, project) {
            (Some(handles), false) if !handles.is_empty() => Recipients::Members(
                handles
                    .iter()
                    .map(|text| handle("to", text))
                    .collect::<Result<_, _>>()?,
            ),
            (None, true) => Recipients::Project,
            (Some(_), true) => {
                return Err(invalid(
                    "to",
                    "give the members in to, or set project, but not both",
                ));
            }
            _ => {
                return Err(invalid(
                    "to",
                    "say who it goes to: the handles in to, or project",
                ));
            }
        };
        let Some(subject) = &self.subject else {
            return Err(invalid("subject", "a new conversation needs a subject"));
        };
        let subject = Subject::parse(subject)
            .map_err(|error| invalid("subject", &format!("the subject is not valid ({error})")))?;
        let item = self.item.as_ref().map(|key| key.key("item")).transpose()?;
        Ok(Sending::Start {
            to,
            subject,
            item,
            body,
            importance,
            ack_required,
        })
    }
}

/// `dibs_inbox`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InboxInput {
    /// Only the conversations with messages you have not read.
    pub unread: Option<bool>,
    /// List, under unacked, the messages that ask you for an acknowledgement you have not given.
    pub unacked: Option<bool>,
}

/// `dibs_ack`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AckInput {
    /// The id of the message to acknowledge.
    pub message: u64,
}

impl AckInput {
    /// The message it names.
    ///
    /// # Errors
    /// A [`Failure`] when the id is zero.
    pub fn id(&self) -> Result<MessageId, Failure> {
        if self.message == 0 {
            return Err(invalid(
                "message",
                "message ids are whole numbers starting at 1",
            ));
        }
        Ok(MessageId(self.message))
    }
}

/// `dibs_thread` and `dibs_read`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ThreadInput {
    /// The conversation's id.
    pub thread: u64,
}

impl ThreadInput {
    /// The conversation it names.
    ///
    /// # Errors
    /// A [`Failure`] when the id is zero.
    pub fn id(&self) -> Result<ThreadId, Failure> {
        thread_id("thread", self.thread)
    }
}

fn thread_id(field: &str, number: u64) -> Result<ThreadId, Failure> {
    if number == 0 {
        return Err(invalid(
            field,
            "conversation ids are whole numbers starting at 1",
        ));
    }
    Ok(ThreadId(number))
}

/// `dibs_slot_take`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SlotTakeInput {
    /// The slot (`deploy:prod`).
    pub slot: String,
    /// Why you take it, as every member reads it; never a credential, a host or a customer's data.
    pub purpose: String,
    /// The work item it is for: its local id or its reference.
    pub item: Option<ItemKeyInput>,
    /// When the slot is held, wait up to this many seconds (at most 540) for it to be offered to
    /// you.
    pub wait_seconds: Option<u64>,
}

/// The longest wait a take may ask for, in seconds.
pub const MAX_WAIT_SECONDS: u64 = 540;

impl SlotTakeInput {
    /// The slot named.
    ///
    /// # Errors
    /// A [`Failure`] when it is no slot name.
    pub fn slot(&self) -> Result<SlotName, Failure> {
        slot_name("slot", &self.slot)
    }

    /// The purpose.
    ///
    /// # Errors
    /// A [`Failure`] when it is empty or too long.
    pub fn purpose(&self) -> Result<Purpose, Failure> {
        Purpose::parse(&self.purpose).map_err(|error| invalid("purpose", &error.to_string()))
    }

    /// The wait asked for.
    ///
    /// # Errors
    /// A [`Failure`] for a wait longer than [`MAX_WAIT_SECONDS`].
    pub fn wait(&self) -> Result<Option<std::time::Duration>, Failure> {
        match self.wait_seconds {
            Some(seconds) if seconds > MAX_WAIT_SECONDS => Err(invalid(
                "wait_seconds",
                "wait at most 540 seconds, under the time a tool call may take",
            )),
            Some(0) | None => Ok(None),
            Some(seconds) => Ok(Some(std::time::Duration::from_secs(seconds))),
        }
    }
}

/// `dibs_slot_release` and `dibs_slot_leave`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SlotInput {
    /// The slot (`deploy:prod`).
    pub slot: String,
}

impl SlotInput {
    /// The slot named.
    ///
    /// # Errors
    /// A [`Failure`] when it is no slot name.
    pub fn slot(&self) -> Result<SlotName, Failure> {
        slot_name("slot", &self.slot)
    }
}

fn slot_name(field: &str, text: &str) -> Result<SlotName, Failure> {
    SlotName::parse(text.trim()).map_err(|error| invalid(field, &format!("'{text}': {error}")))
}

fn body(field: &str, text: &str) -> Result<Body, Failure> {
    Body::parse(text).map_err(|error| invalid(field, &format!("the text is not valid ({error})")))
}

fn handle(field: &str, text: &str) -> Result<Handle, Failure> {
    Handle::parse(text.trim()).map_err(|_| {
        invalid(
            field,
            &format!(
                "'{text}' is not a member handle; handles are lowercase letters, digits and hyphens"
            ),
        )
    })
}

/// The failure of an input value: the field at fault and what is wrong with it.
fn invalid(field: &str, problem: &str) -> Failure {
    Failure::config("invalid_input", sentence(&format!("`{field}`: {problem}")))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn parse<T: serde::de::DeserializeOwned>(value: serde_json::Value) -> T {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn a_key_is_a_number_a_numeric_text_or_a_reference() {
        let key = |value| parse::<KeyInput>(json!({"key": value})).key.key("key");
        assert_eq!(key(json!(12)).unwrap().to_string(), "12");
        assert_eq!(key(json!("12")).unwrap().to_string(), "12");
        assert_eq!(
            key(json!(" gh:acme/api#42 ")).unwrap().to_string(),
            "gh:acme/api#42"
        );
        let bad = key(json!("not a key")).unwrap_err();
        assert!(
            bad.message
                .starts_with("`key`: 'not a key' is not a work item")
        );
        assert!(bad.message.ends_with('.'));
    }

    #[test]
    fn an_unknown_field_is_refused_rather_than_ignored() {
        let misspelt = serde_json::from_value::<ClaimInput>(json!({"key": 1, "area": ["billing"]}));
        assert!(misspelt.is_err());
        assert!(serde_json::from_value::<ClaimInput>(json!({"areas": []})).is_err());
    }

    #[test]
    fn a_wide_claim_needs_a_reason_the_holders_can_read() {
        let wide = parse::<ClaimInput>(
            json!({"key": 4, "wide": {"areas": ["billing", "search"], "reason": "Rename money"}}),
        );
        let request = wide.wide().unwrap().unwrap();
        assert_eq!(request.areas, ["billing", "search"]);
        assert_eq!(request.reason, "Rename money");
        let blank =
            parse::<ClaimInput>(json!({"key": 4, "wide": {"areas": ["billing"], "reason": " "}}));
        assert!(blank.wide().is_err());
        assert!(
            parse::<ClaimInput>(json!({"key": 4}))
                .wide()
                .unwrap()
                .is_none()
        );
        assert!(
            serde_json::from_value::<ClaimInput>(json!({"key": 4, "wide": {"areas": []}})).is_err()
        );
    }

    #[test]
    fn a_claim_without_areas_is_a_claim_of_none() {
        assert!(
            parse::<ClaimInput>(json!({"key": "gh:acme/api#1"}))
                .areas
                .is_empty()
        );
    }

    #[test]
    fn items_filter_by_state_and_by_the_caller() {
        let me: Handle = "alice".parse().unwrap();
        let all = parse::<ItemsInput>(json!({})).filter(&me);
        assert_eq!((all.status, all.holder), (None, None));
        let mine = parse::<ItemsInput>(json!({"status": "claimed", "mine": true})).filter(&me);
        assert_eq!(mine.status, Some(ItemStatus::Claimed));
        assert_eq!(mine.holder, Some(me.clone()));
        let not_mine = parse::<ItemsInput>(json!({"mine": false})).filter(&me);
        assert_eq!(not_mine.holder, None);
        assert!(serde_json::from_value::<ItemsInput>(json!({"status": "finished"})).is_err());
    }

    #[test]
    fn a_new_item_needs_a_reference_and_a_title() {
        let item = parse::<NewInput>(json!({"ref": "gh:acme/api#7", "title": " Fix rounding "}))
            .item()
            .unwrap();
        assert_eq!(item.external_ref.unwrap().to_string(), "gh:acme/api#7");
        assert_eq!(item.title.to_string(), "Fix rounding");
        let bad_ref = parse::<NewInput>(json!({"ref": "nonsense", "title": "x"}))
            .item()
            .unwrap_err();
        assert!(
            bad_ref
                .message
                .starts_with("`ref`: 'nonsense' is not a reference")
        );
        let bad_title = parse::<NewInput>(json!({"ref": "gh:acme/api#7", "title": "  "}))
            .item()
            .unwrap_err();
        assert!(
            bad_title
                .message
                .starts_with("`title`: the title is not valid")
        );
    }

    #[test]
    fn a_takeover_needs_a_reason() {
        let input = parse::<TakeoverInput>(json!({"key": 3, "reason": "  "}));
        assert!(
            input
                .reason()
                .unwrap_err()
                .message
                .starts_with("`reason`: ")
        );
        let input = parse::<TakeoverInput>(json!({"key": 3, "reason": "You went quiet."}));
        assert_eq!(input.reason().unwrap().to_string(), "You went quiet.");
    }

    #[test]
    fn a_check_needs_paths() {
        assert!(parse::<CheckInput>(json!({"paths": []})).paths().is_err());
        assert!(
            parse::<CheckInput>(json!({"paths": [" "]}))
                .paths()
                .is_err()
        );
        let paths = parse::<CheckInput>(json!({"paths": ["src/a.rs", "/abs/b.rs"]}))
            .paths()
            .unwrap();
        assert_eq!(
            paths,
            [PathBuf::from("src/a.rs"), PathBuf::from("/abs/b.rs")]
        );
    }

    #[test]
    fn a_grant_goes_to_a_valid_handle() {
        let item = ItemId(3);
        let ok = parse::<GrantInput>(json!({"key": 3, "area": "api:billing", "to": "bob"}))
            .request(item)
            .unwrap();
        assert_eq!((ok.area.as_str(), ok.to.as_str()), ("api:billing", "bob"));
        let bad = parse::<GrantInput>(json!({"key": 3, "area": "api:billing", "to": "Bob Smith"}))
            .request(item)
            .unwrap_err();
        assert!(
            bad.message
                .starts_with("`to`: 'Bob Smith' is not a member handle")
        );
    }

    #[test]
    fn a_message_starts_a_conversation_or_replies_to_one() {
        let sending = |value| parse::<SendInput>(value).sending();
        let started = sending(json!({
            "to": ["bob", "carol"], "subject": "Rates", "body": "hi", "item": "gh:acme/api#1"
        }))
        .unwrap();
        assert!(matches!(
            started,
            Sending::Start { to: Recipients::Members(ref handles), item: Some(_), .. }
                if handles.len() == 2
        ));
        let project = sending(json!({"project": true, "subject": "Freeze", "body": "hi"})).unwrap();
        assert!(matches!(
            project,
            Sending::Start {
                to: Recipients::Project,
                ..
            }
        ));
        let reply = sending(json!({"thread": 4, "body": "thanks"})).unwrap();
        assert!(matches!(
            reply,
            Sending::Reply {
                thread: ThreadId(4),
                importance: Importance::Normal,
                ack_required: false,
                ..
            }
        ));
        let urgent = sending(json!({
            "thread": 4, "body": "stop", "importance": "urgent", "ack_required": true
        }))
        .unwrap();
        assert!(matches!(
            urgent,
            Sending::Reply {
                importance: Importance::Urgent,
                ack_required: true,
                ..
            }
        ));
        let asked = sending(json!({
            "to": ["bob"], "subject": "Freeze", "body": "hi", "importance": "high", "ack_required": true
        }))
        .unwrap();
        assert!(matches!(
            asked,
            Sending::Start {
                importance: Importance::High,
                ack_required: true,
                ..
            }
        ));
        assert!(
            serde_json::from_value::<SendInput>(json!({"body": "b", "importance": "loud"}))
                .is_err()
        );
    }

    #[test]
    fn a_message_that_is_neither_one_thing_nor_the_other_says_what_to_change() {
        let refused = |value| parse::<SendInput>(value).sending().unwrap_err().message;
        assert!(
            refused(json!({"subject": "s", "body": "b"})).starts_with("`to`: say who it goes to")
        );
        assert!(
            refused(json!({"to": [], "subject": "s", "body": "b"})).starts_with("`to`: say who")
        );
        assert!(
            refused(json!({"to": ["bob"], "project": true, "subject": "s", "body": "b"}))
                .starts_with("`to`: give the members")
        );
        assert!(refused(json!({"to": ["bob"], "body": "b"})).starts_with("`subject`: "));
        assert!(
            refused(json!({"to": ["Bob"], "subject": "s", "body": "b"})).starts_with("`to`: 'Bob'")
        );
        assert!(
            refused(json!({"to": ["bob"], "subject": "s", "body": " "})).starts_with("`body`: ")
        );
        assert!(
            refused(json!({"thread": 4, "subject": "s", "body": "b"}))
                .starts_with("`thread`: a reply takes only")
        );
        assert!(refused(json!({"thread": 0, "body": "b"})).starts_with("`thread`: "));
    }

    #[test]
    fn a_message_to_ack_is_named_by_a_positive_id() {
        assert_eq!(
            parse::<AckInput>(json!({"message": 7})).id().unwrap(),
            MessageId(7)
        );
        assert!(
            parse::<AckInput>(json!({"message": 0}))
                .id()
                .unwrap_err()
                .message
                .starts_with("`message`: ")
        );
        assert!(serde_json::from_value::<AckInput>(json!({"message": 7, "x": 1})).is_err());
    }

    #[test]
    fn a_conversation_id_is_a_positive_whole_number() {
        assert!(parse::<ThreadInput>(json!({"thread": 0})).id().is_err());
        assert_eq!(
            parse::<ThreadInput>(json!({"thread": 9})).id().unwrap(),
            ThreadId(9)
        );
        assert!(serde_json::from_value::<ThreadInput>(json!({"thread": -1})).is_err());
        assert!(serde_json::from_value::<ThreadInput>(json!({"thread": "4"})).is_err());
    }
}

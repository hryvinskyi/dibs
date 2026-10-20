//! Delivery of what reaches a member while they work: a pressing message, or a slot offered to
//! them. A member runs several sessions, and each could put the same message in front of its agent;
//! the first session to claim a subject delivers it, and every other one is told it was delivered.

use std::fmt;

use thiserror::Error;
use time::{Duration, OffsetDateTime};

use crate::ids::{MemberId, MessageId, SessionId};
use crate::slot::SlotName;

/// What a delivery is of.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DeliverySubject {
    /// A message.
    Message(MessageId),
    /// A slot offered to the member until `until`. A new offer after one lapsed is a new subject,
    /// since its deadline differs.
    Offer {
        /// The slot.
        slot: SlotName,
        /// When the offer lapses.
        until: OffsetDateTime,
    },
}

/// Why a text is not a delivery subject.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DeliverySubjectError {
    /// The text is neither a message id nor `offer:<slot>:<deadline>`.
    #[error("a delivery is of a message id or of offer:<slot>:<deadline in unix nanoseconds>")]
    Malformed,
}

impl DeliverySubject {
    /// The prefix of an offer's key.
    const OFFER: &'static str = "offer:";

    /// The subject as one stable text: the message's id (`42`), or
    /// `offer:<slot>:<deadline in unix nanoseconds>`.
    pub fn key(&self) -> String {
        match self {
            Self::Message(message) => message.to_string(),
            Self::Offer { slot, until } => {
                format!("{}{slot}:{}", Self::OFFER, until.unix_timestamp_nanos())
            }
        }
    }

    /// The subject a key names.
    ///
    /// # Errors
    /// [`DeliverySubjectError::Malformed`] for any other text.
    pub fn parse(key: &str) -> Result<Self, DeliverySubjectError> {
        if let Some(rest) = key.strip_prefix(Self::OFFER) {
            let (slot, nanos) = rest
                .rsplit_once(':')
                .ok_or(DeliverySubjectError::Malformed)?;
            let slot = SlotName::parse(slot).map_err(|_| DeliverySubjectError::Malformed)?;
            let nanos: i128 = nanos.parse().map_err(|_| DeliverySubjectError::Malformed)?;
            let until = OffsetDateTime::from_unix_timestamp_nanos(nanos)
                .map_err(|_| DeliverySubjectError::Malformed)?;
            return Ok(Self::Offer { slot, until });
        }
        if key.is_empty() || !key.chars().all(|c| c.is_ascii_digit()) {
            return Err(DeliverySubjectError::Malformed);
        }
        key.parse::<u64>()
            .map(|id| Self::Message(MessageId(id)))
            .map_err(|_| DeliverySubjectError::Malformed)
    }
}

impl fmt::Display for DeliverySubject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.key())
    }
}

/// That one session of a member delivered a subject to its agent: the member's only delivery of
/// it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delivery {
    /// The member it reached.
    pub member: MemberId,
    /// What reached them.
    pub subject: DeliverySubject,
    /// The session that delivers it.
    pub session: SessionId,
    /// When it was claimed.
    pub at: OffsetDateTime,
}

/// How long a message about a work item is kept for the session that holds the item's claim, from
/// when it was posted: the agent working on the item hears it rather than a neighbouring session.
pub const RESERVED_FOR: Duration = Duration::seconds(60);

/// How recently a session that can be woken while idle must have announced itself for a message to
/// be kept for it.
pub const PUSH_SEEN_WITHIN: Duration = Duration::minutes(3);

/// How often a session that can be woken while idle announces itself while it runs, well within
/// [`PUSH_SEEN_WITHIN`].
pub const PUSH_ANNOUNCE_EVERY: Duration = Duration::minutes(1);

/// The session that took or last renewed the member's live claim on the item a message is about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimSession {
    /// The session.
    pub session: SessionId,
    /// Whether it can be woken while idle.
    pub push: bool,
    /// When it last announced itself.
    pub last_seen: OffsetDateTime,
}

/// That a message is kept, until `until`, for the one session that may deliver it meanwhile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reservation {
    /// The session it is kept for.
    pub session: SessionId,
    /// When anyone of the member's sessions may deliver it again.
    pub until: OffsetDateTime,
}

/// The reservation of a message posted at `posted_at` about an item whose claim, held by the
/// recipient, `holder` took: kept for that session for [`RESERVED_FOR`], when it can be woken while
/// idle and announced itself within [`PUSH_SEEN_WITHIN`] of `now`. A session seen in the future
/// (another clock running ahead) counts as seen. `None` when nothing is kept.
pub fn reservation(
    posted_at: OffsetDateTime,
    holder: Option<&ClaimSession>,
    now: OffsetDateTime,
) -> Option<Reservation> {
    let holder =
        holder.filter(|holder| holder.push && now - holder.last_seen <= PUSH_SEEN_WITHIN)?;
    let until = posted_at + RESERVED_FOR;
    (now < until).then_some(Reservation {
        session: holder.session,
        until,
    })
}

/// Why a session may not deliver a subject.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DeliveryRefused {
    /// The message does not reach the member: they wrote it, or it is not addressed to them.
    #[error("this message was not sent to you, so it is not yours to deliver")]
    NotRecipient,
    /// The slot is not on offer to the member until that deadline any more: the offer was taken,
    /// lapsed, or was never theirs.
    #[error("this slot is no longer offered to you until that time")]
    OfferGone,
    /// Another delivery of the subject to the member was claimed first, by this session or
    /// another of theirs.
    #[error("this was already delivered to you, in another session or earlier in this one")]
    Delivered {
        /// The session that claimed it.
        session: SessionId,
        /// When.
        at: OffsetDateTime,
    },
    /// The message is about a work item another session of the member works on, and is kept for
    /// that session for a while.
    #[error("this message is kept for a while for the session of yours that works on its item")]
    Reserved {
        /// When it may be delivered by any session of the member.
        until: OffsetDateTime,
    },
}

/// Decides whether `session` of `member` may deliver `subject` at `now`, given the member's
/// delivery of it so far and the reservation that keeps it for one session: the first claim wins,
/// and every claim after it is refused, whichever session makes it; while a reservation stands,
/// only its session may make the first.
///
/// # Errors
/// [`DeliveryRefused::Delivered`] when a delivery of the subject to the member exists;
/// [`DeliveryRefused::Reserved`] while it is kept for another session.
pub fn decide_delivery(
    existing: Option<&Delivery>,
    reserved: Option<&Reservation>,
    member: MemberId,
    subject: DeliverySubject,
    session: SessionId,
    now: OffsetDateTime,
) -> Result<Delivery, DeliveryRefused> {
    if let Some(first) = existing {
        return Err(DeliveryRefused::Delivered {
            session: first.session,
            at: first.at,
        });
    }
    if let Some(kept) = reserved.filter(|kept| kept.session != session && now < kept.until) {
        return Err(DeliveryRefused::Reserved { until: kept.until });
    }
    Ok(Delivery {
        member,
        subject,
        session,
        at: now,
    })
}

#[cfg(test)]
mod tests {
    use time::Duration;

    use super::*;

    fn offer(seconds: i64) -> DeliverySubject {
        DeliverySubject::Offer {
            slot: SlotName::parse("deploy:prod").unwrap(),
            until: OffsetDateTime::UNIX_EPOCH + Duration::seconds(seconds),
        }
    }

    #[test]
    fn a_subject_reads_back_from_its_key() {
        let message = DeliverySubject::Message(MessageId(42));
        assert_eq!(message.key(), "42");
        assert_eq!(DeliverySubject::parse("42").unwrap(), message);
        let offered = offer(90);
        assert_eq!(offered.key(), "offer:deploy:prod:90000000000");
        assert_eq!(DeliverySubject::parse(&offered.key()).unwrap(), offered);
        for bad in [
            "",
            "-1",
            "4a",
            "offer:",
            "offer:deploy",
            "offer:Bad Slot:1",
            "offer:x:y",
        ] {
            assert!(DeliverySubject::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_new_offer_after_a_lapse_is_a_new_subject() {
        assert_ne!(offer(90).key(), offer(400).key());
    }

    #[test]
    fn the_first_claim_wins_and_every_later_one_is_refused() {
        let now = OffsetDateTime::UNIX_EPOCH + Duration::days(1);
        let first = decide_delivery(
            None,
            None,
            MemberId(1),
            DeliverySubject::Message(MessageId(7)),
            SessionId(10),
            now,
        )
        .unwrap();
        assert_eq!(first.session, SessionId(10));
        for session in [SessionId(10), SessionId(11)] {
            assert_eq!(
                decide_delivery(
                    Some(&first),
                    None,
                    MemberId(1),
                    DeliverySubject::Message(MessageId(7)),
                    session,
                    now + Duration::seconds(1),
                ),
                Err(DeliveryRefused::Delivered {
                    session: SessionId(10),
                    at: now
                })
            );
        }
    }

    fn holder(push: bool, seen_ago: i64) -> ClaimSession {
        ClaimSession {
            session: SessionId(20),
            push,
            last_seen: OffsetDateTime::UNIX_EPOCH + Duration::days(1) - Duration::seconds(seen_ago),
        }
    }

    #[test]
    fn a_message_about_an_item_is_kept_for_a_minute_for_the_running_session_that_works_on_it() {
        let posted = OffsetDateTime::UNIX_EPOCH + Duration::days(1);
        let kept = reservation(posted, Some(&holder(true, 30)), posted).unwrap();
        assert_eq!(
            kept,
            Reservation {
                session: SessionId(20),
                until: posted + RESERVED_FOR
            }
        );
        assert_eq!(RESERVED_FOR, Duration::seconds(60));
        assert!(
            reservation(
                posted,
                Some(&holder(true, 30)),
                posted + Duration::seconds(59)
            )
            .is_some()
        );
        assert_eq!(
            reservation(
                posted,
                Some(&holder(true, 30)),
                posted + Duration::seconds(60)
            ),
            None,
            "the window is over"
        );
        assert_eq!(
            reservation(posted, Some(&holder(false, 0)), posted),
            None,
            "a session that cannot be woken while idle hears it between its tool calls anyway"
        );
        assert_eq!(
            reservation(posted, Some(&holder(true, 181)), posted),
            None,
            "a session that stopped announcing itself has most likely ended"
        );
        assert!(
            reservation(posted, Some(&holder(true, -600)), posted).is_some(),
            "a clock ahead"
        );
        assert_eq!(reservation(posted, None, posted), None);
        assert!(PUSH_ANNOUNCE_EVERY < PUSH_SEEN_WITHIN);
    }

    #[test]
    fn while_a_message_is_kept_only_its_session_delivers_it_and_after_the_window_anyone_may() {
        let posted = OffsetDateTime::UNIX_EPOCH + Duration::days(1);
        let kept = Reservation {
            session: SessionId(20),
            until: posted + RESERVED_FOR,
        };
        let subject = DeliverySubject::Message(MessageId(7));
        let decide = |session: SessionId, at: OffsetDateTime| {
            decide_delivery(None, Some(&kept), MemberId(1), subject.clone(), session, at)
        };
        assert_eq!(
            decide(SessionId(21), posted + Duration::seconds(10)),
            Err(DeliveryRefused::Reserved { until: kept.until })
        );
        assert_eq!(
            decide(SessionId(20), posted + Duration::seconds(10))
                .unwrap()
                .session,
            SessionId(20)
        );
        assert_eq!(
            decide(SessionId(21), kept.until).unwrap().session,
            SessionId(21)
        );
        let first = decide(SessionId(20), posted).unwrap();
        assert!(
            matches!(
                decide_delivery(
                    Some(&first),
                    Some(&kept),
                    MemberId(1),
                    subject.clone(),
                    SessionId(20),
                    posted
                ),
                Err(DeliveryRefused::Delivered { .. })
            ),
            "once delivered, never again, the kept session included"
        );
    }
}

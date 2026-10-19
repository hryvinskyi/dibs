//! A member of a project: what they may do, the name they are shown by, and their login elsewhere.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::text::{has_control, string_value};

/// A member's role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// A person who also manages the project: members, tokens, settings, forced releases.
    Admin,
    /// A person working in the project.
    Member,
    /// An integration bot that feeds the project from an outside tracker. It is never a person: it
    /// holds no claim, receives no message and reads no conversation.
    Importer,
}

impl Role {
    /// Whether the role belongs to a person rather than an integration.
    pub const fn is_person(self) -> bool {
        matches!(self, Self::Admin | Self::Member)
    }

    /// Whether the role may manage the project.
    pub const fn is_admin(self) -> bool {
        matches!(self, Self::Admin)
    }

    /// Whether the role may feed the project's items from an outside tracker: an admin, or the
    /// integration that imports them.
    pub const fn may_import(self) -> bool {
        matches!(self, Self::Admin | Self::Importer)
    }
}

/// The name a member is shown by: 1 to 100 characters after trimming, with no control characters.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct DisplayName(String);

/// Why a text is not a display name.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DisplayNameError {
    /// The trimmed text is empty, longer than 100 characters, or has control characters.
    #[error("a display name is 1 to 100 characters with no control characters")]
    Malformed,
}

impl DisplayName {
    /// Parses a display name, trimming whitespace at both ends.
    ///
    /// # Errors
    /// [`DisplayNameError::Malformed`] when the trimmed text breaks the rules above.
    pub fn parse(value: &str) -> Result<Self, DisplayNameError> {
        let value = value.trim();
        if value.is_empty() || value.chars().count() > 100 || has_control(value) {
            return Err(DisplayNameError::Malformed);
        }
        Ok(Self(value.to_owned()))
    }
}

string_value!(DisplayName, DisplayNameError);

/// A member's login in an outside system (a forge account an integration maps to the member): 1 to
/// 100 characters with no whitespace or control characters.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ExternalLogin(String);

/// Why a text is not an outside login.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ExternalLoginError {
    /// The text is empty, longer than 100 characters, or has whitespace or control characters.
    #[error("an external login is 1 to 100 characters with no spaces or control characters")]
    Malformed,
}

impl ExternalLogin {
    /// Parses an outside login.
    ///
    /// # Errors
    /// [`ExternalLoginError::Malformed`] when the text breaks the rules above.
    pub fn parse(value: &str) -> Result<Self, ExternalLoginError> {
        let ok = !value.is_empty()
            && value.chars().count() <= 100
            && !has_control(value)
            && !value.chars().any(char::is_whitespace);
        if ok {
            Ok(Self(value.to_owned()))
        } else {
            Err(ExternalLoginError::Malformed)
        }
    }
}

string_value!(ExternalLogin, ExternalLoginError);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_admins_and_members_are_people() {
        assert!(Role::Admin.is_person());
        assert!(Role::Member.is_person());
        assert!(!Role::Importer.is_person());
        assert!(Role::Admin.is_admin());
        assert!(!Role::Member.is_admin());
        assert!(!Role::Importer.is_admin());
        assert!(Role::Admin.may_import());
        assert!(Role::Importer.may_import());
        assert!(!Role::Member.may_import());
    }

    #[test]
    fn display_name_is_trimmed_and_bounded() {
        assert_eq!(DisplayName::parse("  Alice  ").unwrap().as_str(), "Alice");
        assert!(DisplayName::parse(&"a".repeat(100)).is_ok());
        assert_eq!(
            DisplayName::parse(&"a".repeat(101)),
            Err(DisplayNameError::Malformed)
        );
        assert_eq!(DisplayName::parse("   "), Err(DisplayNameError::Malformed));
        assert_eq!(DisplayName::parse("a\nb"), Err(DisplayNameError::Malformed));
    }

    #[test]
    fn external_login_has_no_spaces() {
        assert!(ExternalLogin::parse("alice-gh").is_ok());
        assert_eq!(
            ExternalLogin::parse("alice gh"),
            Err(ExternalLoginError::Malformed)
        );
        assert_eq!(ExternalLogin::parse(""), Err(ExternalLoginError::Malformed));
        assert_eq!(
            ExternalLogin::parse(&"a".repeat(101)),
            Err(ExternalLoginError::Malformed)
        );
        assert_eq!(
            ExternalLogin::parse("a\u{0}"),
            Err(ExternalLoginError::Malformed)
        );
    }
}

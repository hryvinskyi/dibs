//! What a member may do in their project.

use serde::{Deserialize, Serialize};

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
}

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
    }
}

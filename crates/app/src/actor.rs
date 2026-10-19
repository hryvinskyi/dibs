//! Who is asking: the member a token or browser session authenticated.

use dibs_domain::ids::{MemberId, ProjectId};
use dibs_domain::member::Role;

use crate::error::AppError;
use crate::token::TokenHash;

/// The authenticated caller. Identity is never self-asserted: an actor is the result of checking a
/// token or a browser session, and whatever is done for it is done inside its project only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Actor {
    /// The project the caller belongs to.
    pub project: ProjectId,
    /// The member.
    pub member: MemberId,
    /// The member's role.
    pub role: Role,
    /// The hash of the token that authenticated the caller.
    pub token: TokenHash,
}

impl Actor {
    /// Refuses an integration bot: only people hold claims, grant, and write or read messages.
    ///
    /// # Errors
    /// [`AppError::Forbidden`] for a role that is not a person's.
    pub fn require_person(&self) -> Result<(), AppError> {
        if self.role.is_person() {
            Ok(())
        } else {
            Err(AppError::Forbidden)
        }
    }

    /// Refuses anyone who may not feed the project's items from an outside tracker: only an admin
    /// and the importer may.
    ///
    /// # Errors
    /// [`AppError::Forbidden`] for a member.
    pub fn require_import(&self) -> Result<(), AppError> {
        if self.role.may_import() {
            Ok(())
        } else {
            Err(AppError::Forbidden)
        }
    }

    /// Refuses anyone but an admin.
    ///
    /// # Errors
    /// [`AppError::Forbidden`] for a role that is not admin.
    pub fn require_admin(&self) -> Result<(), AppError> {
        if self.role.is_admin() {
            Ok(())
        } else {
            Err(AppError::Forbidden)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn actor(role: Role) -> Actor {
        Actor {
            project: ProjectId(1),
            member: MemberId(2),
            role,
            token: TokenHash::new("h".to_owned()),
        }
    }

    #[test]
    fn people_and_admins_are_told_apart() {
        assert!(actor(Role::Admin).require_person().is_ok());
        assert!(actor(Role::Member).require_person().is_ok());
        assert!(matches!(
            actor(Role::Importer).require_person(),
            Err(AppError::Forbidden)
        ));
        assert!(actor(Role::Admin).require_admin().is_ok());
        assert!(matches!(
            actor(Role::Member).require_admin(),
            Err(AppError::Forbidden)
        ));
        assert!(matches!(
            actor(Role::Importer).require_admin(),
            Err(AppError::Forbidden)
        ));
        assert!(actor(Role::Admin).require_import().is_ok());
        assert!(actor(Role::Importer).require_import().is_ok());
        assert!(matches!(
            actor(Role::Member).require_import(),
            Err(AppError::Forbidden)
        ));
    }
}

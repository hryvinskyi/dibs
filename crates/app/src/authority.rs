//! Who may administer a project: an admin a token authenticated, or the operator of the service.

use dibs_domain::ids::{MemberId, ProjectId};

use crate::actor::Actor;
use crate::error::AppError;

/// The right to administer one project: to add its members and replace their tokens.
pub trait AdminAuthority: Send + Sync {
    /// The project this authority administers.
    ///
    /// # Errors
    /// [`AppError::Forbidden`] when the holder may not administer it.
    fn administered_project(&self) -> Result<ProjectId, AppError>;

    /// The admin acting, as the records name them; `None` for the operator of the service, who is
    /// no member.
    fn acting_member(&self) -> Option<MemberId>;
}

impl AdminAuthority for Actor {
    /// The caller's own project, and only for an admin.
    fn administered_project(&self) -> Result<ProjectId, AppError> {
        self.require_admin()?;
        Ok(self.project)
    }

    fn acting_member(&self) -> Option<MemberId> {
        Some(self.member)
    }
}

/// The operator of the service acting on one project. The operator is whoever runs the service's
/// own command line against its database file: the authority comes from access to that file, not
/// from a token, so it reaches a project whose every admin lost their token. Only the use case that
/// finds the project by its slug issues one, so an operator always names a project that exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Operator {
    project: ProjectId,
}

impl Operator {
    /// The operator of `project`.
    pub(crate) fn of(project: ProjectId) -> Self {
        Self { project }
    }

    /// The project acted on.
    pub fn project(&self) -> ProjectId {
        self.project
    }
}

impl AdminAuthority for Operator {
    fn administered_project(&self) -> Result<ProjectId, AppError> {
        Ok(self.project)
    }

    fn acting_member(&self) -> Option<MemberId> {
        None
    }
}

#[cfg(test)]
mod tests {
    use dibs_domain::member::Role;

    use super::*;
    use crate::token::TokenHash;

    fn actor(role: Role) -> Actor {
        Actor {
            project: ProjectId(4),
            member: MemberId(2),
            role,
            token: TokenHash::new("h".to_owned()),
        }
    }

    #[test]
    fn an_admin_administers_their_own_project_and_no_one_else_does() {
        assert_eq!(
            actor(Role::Admin).administered_project().unwrap(),
            ProjectId(4)
        );
        assert!(matches!(
            actor(Role::Member).administered_project(),
            Err(AppError::Forbidden)
        ));
        assert!(matches!(
            actor(Role::Importer).administered_project(),
            Err(AppError::Forbidden)
        ));
    }

    #[test]
    fn the_operator_administers_the_project_it_was_issued_for() {
        let operator = Operator::of(ProjectId(9));
        assert_eq!(operator.project(), ProjectId(9));
        assert_eq!(operator.administered_project().unwrap(), ProjectId(9));
        assert_eq!(operator.acting_member(), None, "the operator is no member");
        assert_eq!(actor(Role::Admin).acting_member(), Some(MemberId(2)));
    }
}

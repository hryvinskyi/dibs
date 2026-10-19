//! Steps several use cases share: finding the caller's session and an item, resolving the areas a
//! person named, issuing a token, ending a claim.

use std::collections::BTreeSet;
use std::sync::Arc;

use time::OffsetDateTime;

use dibs_domain::area::QualifiedArea;
use dibs_domain::claim::Claim;
use dibs_domain::handle::Handle;
use dibs_domain::ids::{ItemId, MemberId, SessionId};
use dibs_domain::member::Role;
use dibs_domain::settings::ProjectSettings;

use crate::actor::Actor;
use crate::error::{AppError, Duplicate, Missing};
use crate::ports::{IdGen, TokenGenerator, TokenHasher};
use crate::records::{ItemRecord, MemberRecord, SessionRecord, TokenRecord};
use crate::token::SecretToken;
use crate::work::Work;
use dibs_domain::member::DisplayName;
use dibs_domain::member::ExternalLogin;

/// The caller's session of this id. A session of another member, or of another project, is reported
/// as missing.
pub(crate) async fn own_session(
    work: &mut Work,
    actor: &Actor,
    id: SessionId,
) -> Result<SessionRecord, AppError> {
    let project = work.project_id();
    work.session(project, id)
        .await?
        .filter(|session| session.member == actor.member)
        .ok_or(AppError::NotFound(Missing::Session(id)))
}

/// The item of this id in the transaction's project.
pub(crate) async fn existing_item(work: &mut Work, id: ItemId) -> Result<ItemRecord, AppError> {
    let project = work.project_id();
    work.item(project, id)
        .await?
        .ok_or(AppError::NotFound(Missing::Item(id)))
}

/// The member of this handle in the transaction's project.
pub(crate) async fn member_by_handle(
    work: &mut Work,
    handle: &Handle,
) -> Result<MemberRecord, AppError> {
    let project = work.project_id();
    work.member_by_handle(project, handle)
        .await?
        .ok_or_else(|| AppError::NotFound(Missing::Member(handle.clone())))
}

/// Resolves areas as a person names them — `repo:area`, or a bare name one repository declares —
/// to their qualified names.
pub(crate) fn resolve_areas(
    settings: &ProjectSettings,
    texts: &[String],
) -> Result<BTreeSet<QualifiedArea>, AppError> {
    texts
        .iter()
        .map(|text| settings.resolve_area(text).map_err(AppError::from))
        .collect()
}

/// Removes a claim together with the paths recorded as shared under it.
pub(crate) async fn remove_claim(work: &mut Work, claim: &Claim) -> Result<(), AppError> {
    let project = work.project_id();
    work.delete_shared_paths(project, claim.item).await?;
    work.delete_claim(project, claim.item).await?;
    Ok(())
}

/// A member about to join a project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewMember {
    /// The handle, unique within the project.
    pub handle: Handle,
    /// The name it is shown by.
    pub display_name: DisplayName,
    /// Its login in an outside system, when it has one.
    pub external_login: Option<ExternalLogin>,
}

/// Adds members to a project and issues their tokens.
#[derive(Clone)]
pub(crate) struct Enrolment {
    pub(crate) ids: Arc<dyn IdGen>,
    pub(crate) tokens: Arc<dyn TokenGenerator>,
    pub(crate) hasher: Arc<dyn TokenHasher>,
}

impl Enrolment {
    /// Stores a new member of the transaction's project, refusing a handle already taken there.
    pub(crate) async fn add_member(
        &self,
        work: &mut Work,
        new: NewMember,
        role: Role,
        at: OffsetDateTime,
    ) -> Result<MemberRecord, AppError> {
        let project = work.project_id();
        if work.member_by_handle(project, &new.handle).await?.is_some() {
            return Err(AppError::Duplicate(Duplicate::Handle(new.handle)));
        }
        let member = MemberRecord {
            id: MemberId(self.ids.next()),
            handle: new.handle,
            display_name: new.display_name,
            external_login: new.external_login,
            role,
            created_at: at,
        };
        work.insert_member(project, &member).await?;
        Ok(member)
    }

    /// Issues a token for a member of the transaction's project and stores its hash. The secret is
    /// returned once and kept nowhere.
    pub(crate) async fn issue_token(
        &self,
        work: &mut Work,
        member: MemberId,
        at: OffsetDateTime,
    ) -> Result<SecretToken, AppError> {
        let project = work.project_id();
        let token = self.tokens.generate();
        let record = TokenRecord {
            hash: self.hasher.hash(&token),
            prefix: token.prefix(),
            member,
            created_at: at,
            revoked_at: None,
        };
        work.insert_token(project, &record).await?;
        Ok(token)
    }
}

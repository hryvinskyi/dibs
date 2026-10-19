//! Announcing a running agent process, MCP server or browser.

use std::sync::Arc;

use dibs_domain::ids::SessionId;
use dibs_domain::message::SessionLabel;
use dibs_domain::session::HostName;

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::ports::{Clock, IdGen, Store};
use crate::records::SessionRecord;
use crate::work::Work;
use dibs_domain::session::AgentLabel;

/// The session as it describes itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisterSessionInput {
    /// The machine it runs on.
    pub host: HostName,
    /// Its label, unique per member.
    pub label: SessionLabel,
    /// The kind of agent it runs.
    pub agent: AgentLabel,
    /// Whether it can be woken while idle.
    pub push: bool,
}

/// Registers a session of the caller, or updates the one of the same label: a session that
/// registers again keeps its id.
pub struct RegisterSession {
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGen>,
}

impl RegisterSession {
    /// Builds the use case.
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            store: deps.store.clone(),
            clock: deps.clock.clone(),
            ids: deps.ids.clone(),
        }
    }

    /// Registers the session.
    ///
    /// # Errors
    /// [`AppError::Forbidden`] for an integration; [`AppError::Store`] when the store fails.
    pub async fn execute(
        &self,
        actor: &Actor,
        input: RegisterSessionInput,
    ) -> Result<SessionRecord, AppError> {
        actor.require_person()?;
        let now = self.clock.now();
        let mut work = Work::begin(&*self.store, actor.project).await?;
        let known = work
            .session_by_label(actor.project, actor.member, &input.label)
            .await?;
        let id = known
            .map(|session| session.id)
            .unwrap_or_else(|| SessionId(self.ids.next()));
        let session = SessionRecord {
            id,
            member: actor.member,
            host: input.host,
            label: input.label,
            agent: input.agent,
            push: input.push,
            last_seen: now,
        };
        work.save_session(actor.project, &session).await?;
        work.commit_quietly().await?;
        Ok(session)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::use_cases::testing::Env;

    fn input(host: &str, label: &str, push: bool) -> RegisterSessionInput {
        RegisterSessionInput {
            host: HostName::parse(host).unwrap(),
            label: SessionLabel::parse(label).unwrap(),
            agent: AgentLabel::parse("agent").unwrap(),
            push,
        }
    }

    #[tokio::test]
    async fn a_label_registers_once_per_member() {
        let env = Env::new().await;
        let register = RegisterSession::new(&env.deps);
        let first = register
            .execute(&env.bob.actor, input("desktop", "desktop/mcp", false))
            .await
            .unwrap();
        env.advance(time::Duration::minutes(3));
        let again = register
            .execute(&env.bob.actor, input("desktop-2", "desktop/mcp", true))
            .await
            .unwrap();
        assert_eq!(again.id, first.id);
        assert_eq!(again.host.as_str(), "desktop-2");
        assert!(again.push);
        assert!(again.last_seen > first.last_seen);
        let alices = register
            .execute(&env.alice.actor, input("laptop", "desktop/mcp", false))
            .await
            .unwrap();
        assert_ne!(alices.id, first.id);
        let mut tx = env.deps.store.begin().await.unwrap();
        let stored = tx.session(env.project, first.id).await.unwrap().unwrap();
        assert_eq!(stored, again);
        drop(tx);
        assert!(
            env.events()
                .await
                .iter()
                .all(|e| !matches!(e.kind, dibs_domain::event::EventKind::Claimed(_)))
        );
    }
}

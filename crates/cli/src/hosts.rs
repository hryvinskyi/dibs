//! The agent hosts this binary supports, built from the adapter crates it links. Adding a host is
//! one more entry here and the adapter crate it comes from; nothing else in the binary changes.

use dibs_agent_host::HostRegistry;
use dibs_agent_host::neutral::Neutral;

/// The registry of every linked host.
pub fn registry() -> HostRegistry {
    HostRegistry::new(vec![Box::new(Neutral)])
}

#[cfg(test)]
mod tests {
    use dibs_domain::session::AgentLabel;

    use super::*;

    #[test]
    fn every_linked_host_has_an_id_its_sessions_can_report() {
        let registry = registry();
        for adapter in registry.adapters() {
            assert!(AgentLabel::parse(adapter.id().as_str()).is_ok());
            assert!(registry.get(adapter.id().as_str()).is_some());
        }
        assert!(registry.get("neutral").is_some());
    }
}

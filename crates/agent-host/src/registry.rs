//! The hosts a binary links, looked up by id.

use crate::adapter::HostAdapter;

/// The adapters of the hosts a binary supports. Built once by the composition root from the
/// adapter crates it links; adding a host adds an entry and changes nothing else.
pub struct HostRegistry {
    adapters: Vec<Box<dyn HostAdapter>>,
}

impl HostRegistry {
    /// A registry of these adapters. When two share an id, the first one answers for it.
    pub fn new(adapters: Vec<Box<dyn HostAdapter>>) -> Self {
        Self { adapters }
    }

    /// The adapter of the host with this id.
    pub fn get(&self, id: &str) -> Option<&dyn HostAdapter> {
        self.adapters
            .iter()
            .find(|adapter| adapter.id().as_str() == id)
            .map(AsRef::as_ref)
    }

    /// Every adapter, in the order given.
    pub fn adapters(&self) -> impl Iterator<Item = &dyn HostAdapter> {
        self.adapters.iter().map(AsRef::as_ref)
    }

    /// The ids, in the order given, joined for a sentence: `a, b`.
    pub fn ids(&self) -> String {
        self.adapters()
            .map(|adapter| adapter.id().as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::neutral::Neutral;

    #[test]
    fn a_host_is_found_by_its_id_only() {
        let registry = HostRegistry::new(vec![Box::new(Neutral)]);
        assert!(registry.get("neutral").is_some());
        assert!(registry.get("Neutral").is_none());
        assert!(registry.get("elsewhere").is_none());
        assert_eq!(registry.ids(), "neutral");
    }
}

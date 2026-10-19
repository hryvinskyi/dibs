//! The ports and registries a composition root hands to the use cases.

use std::sync::Arc;

use dibs_domain::hot_rule::HotRuleRegistry;
use dibs_domain::ref_scheme::RefSchemeRegistry;

use crate::ports::{Clock, EventNotifier, IdGen, Store, TokenGenerator, TokenHasher};

/// Everything a use case may be built from. Each use case keeps only the parts it uses.
#[derive(Clone)]
pub struct Dependencies {
    /// The store.
    pub store: Arc<dyn Store>,
    /// The clock.
    pub clock: Arc<dyn Clock>,
    /// The id generator.
    pub ids: Arc<dyn IdGen>,
    /// The secret generator.
    pub tokens: Arc<dyn TokenGenerator>,
    /// The secret hasher.
    pub hasher: Arc<dyn TokenHasher>,
    /// Wakes the followers of a project's event log after a commit.
    pub notifier: Arc<dyn EventNotifier>,
    /// The hot-file rule kinds settings may use.
    pub hot_rules: Arc<HotRuleRegistry>,
    /// The reference schemes whose values name a repository.
    pub ref_schemes: Arc<RefSchemeRegistry>,
}

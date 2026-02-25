//! Common type filtering for type-edge consumers
//!
//! Provides the `COMMON_TYPES` set used to filter out standard library types
//! from type-edge queries. Without this filter, queries like `get_type_users_batch("String")`
//! would return most of the codebase.

use std::collections::HashSet;
use std::sync::LazyLock;

use crate::language::REGISTRY;

/// Standard library types to exclude from type-edge analysis.
///
/// Built as a union of all enabled languages' `common_types` sets at runtime.
/// Used by `related`, `impact --include-types`, and `read --focus` to prevent
/// common types like `String`, `Vec`, `Result` from dominating results.
pub static COMMON_TYPES: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    let mut set = HashSet::new();
    for def in REGISTRY.all() {
        set.extend(def.common_types.iter().copied());
    }
    set
});

//! Common type filtering for type-edge consumers
//!
//! Provides the `COMMON_TYPES` set used to filter out standard library types
//! from type-edge queries. Without this filter, queries like `get_type_users_batch("String")`
//! would return most of the codebase.

use std::collections::HashSet;
use std::sync::LazyLock;

/// Standard library types to exclude from type-edge analysis.
///
/// Used by `related`, `impact --include-types`, and `read --focus` to prevent
/// common types like `String`, `Vec`, `Result` from dominating results.
pub static COMMON_TYPES: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "String",
        "Vec",
        "Result",
        "Option",
        "Box",
        "Arc",
        "Rc",
        "HashMap",
        "HashSet",
        "BTreeMap",
        "BTreeSet",
        "Path",
        "PathBuf",
        "Value",
        "Error",
        "Self",
        "None",
        "Some",
        "Ok",
        "Err",
        "Mutex",
        "RwLock",
        "Cow",
        "Pin",
        "Future",
        "Iterator",
        "Display",
        "Debug",
        "Clone",
        "Default",
        "Send",
        "Sync",
        "Sized",
        "Copy",
        "From",
        "Into",
        "AsRef",
        "AsMut",
        "Deref",
        "DerefMut",
        "Read",
        "Write",
        "Seek",
        "BufRead",
        "ToString",
        "Serialize",
        "Deserialize",
    ]
    .into_iter()
    .collect()
});

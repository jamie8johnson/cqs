//! Tiny serde predicates for `#[serde(skip_serializing_if = ...)]`.
//!
//! `serde(skip_serializing_if)` requires a free function with the signature
//! `fn(&T) -> bool`; closures and trait methods don't qualify. Rather than
//! redeclare these one-line helpers in every envelope-shaped module
//! (P3-11 from the v1.33.0 audit cataloged six copies), centralize them
//! here so new envelope additions are a single attribute import.
//!
//! Use as:
//! ```ignore
//! #[serde(skip_serializing_if = "crate::serde_helpers::is_false")]
//! pub flag: bool,
//! ```

#[inline]
pub fn is_false(v: &bool) -> bool {
    !*v
}

#[inline]
pub fn is_true(v: &bool) -> bool {
    *v
}

#[inline]
pub fn is_zero_usize(n: &usize) -> bool {
    *n == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_false_correct() {
        assert!(is_false(&false));
        assert!(!is_false(&true));
    }

    #[test]
    fn is_true_correct() {
        assert!(is_true(&true));
        assert!(!is_true(&false));
    }

    #[test]
    fn is_zero_usize_correct() {
        assert!(is_zero_usize(&0));
        assert!(!is_zero_usize(&1));
        assert!(!is_zero_usize(&usize::MAX));
    }
}

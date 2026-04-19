//! Evaluation surface — shared types for query sets, gold chunks, and report rows.
//!
//! Single source of truth for the on-disk eval JSON shape consumed by both
//! the production runner (`src/cli/commands/eval/runner.rs`) and the
//! integration tests in `tests/`. Adding a field here is the **only** place
//! it needs to be added; downstream call sites borrow these types via
//! `cqs::eval::schema::*`.

pub mod schema;

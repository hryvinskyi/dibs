//! Pure domain model: types, value objects and invariants, free of I/O.
//!
//! Every decision here is a function of the values it is given — the current time and every id
//! come in as arguments — and answers with a result value; nothing reads a clock, a random source,
//! a file or the network, and nothing mutates hidden state.

pub mod area;
pub mod claim;
pub mod edit;
pub mod event;
pub mod grant;
pub mod handle;
pub mod hot_rule;
pub mod ids;
pub mod item;
pub mod member;
pub mod message;
pub mod paths;
pub mod ref_scheme;
pub mod repo;
pub mod reservation;
pub mod session;
pub mod settings;
mod text;

#[cfg(test)]
mod testing;

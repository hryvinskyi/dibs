//! The client side of dibs: the typed HTTP client of its API, a checkout's `.dibs.toml` and the
//! branch it is on, the member's credentials, what the client remembers on this machine between
//! runs, and the verified replacement of the installed binary by a newer release.

pub mod api;
pub mod branch;
pub mod checkout;
pub mod client;
pub mod credentials;
pub mod error;
pub mod locations;
pub mod private_file;
pub mod server_url;
pub mod state;
pub mod token;
pub mod update;
pub mod verdicts;

mod sse;

pub use client::{Client, ClientOptions, USER_AGENT};
pub use error::{ClientError, ErrorDetails};

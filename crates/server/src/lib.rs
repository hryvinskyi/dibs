//! HTTP API and composition root of the server.
//!
//! The routes translate HTTP to the use cases of `dibs-app` and back: they parse what a request
//! carries into the values the use cases take, call one use case, and write its answer or refusal
//! as JSON. They decide nothing the use cases decide.

pub mod adapters;
pub mod auth;
pub mod client_ip;
pub mod config;
pub mod error;
pub mod fields;
pub mod http;
pub mod rate_limit;
pub mod routes;
pub mod serve;
pub mod views;

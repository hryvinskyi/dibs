//! The HTTP API through the real router over a real SQLite file: every route's answer and its
//! refusals, sign-in and its limit, tenancy, and the error shape.

#[cfg(test)]
mod support;

#[cfg(test)]
mod auth;
#[cfg(test)]
mod claims;
#[cfg(test)]
mod edit_check;
#[cfg(test)]
mod items;
#[cfg(test)]
mod sessions;
#[cfg(test)]
mod tenancy;
#[cfg(test)]
mod transport;

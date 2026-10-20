//! The HTTP API through the real router over a real SQLite file: every route's answer and its
//! refusals, sign-in and its limit, tenancy, the error shape, and the event stream over a real
//! connection.

#[cfg(test)]
mod support;

#[cfg(test)]
mod acks;
#[cfg(test)]
mod admin;
#[cfg(test)]
mod auth;
#[cfg(test)]
mod claims;
#[cfg(test)]
mod deliveries;
#[cfg(test)]
mod edit_check;
#[cfg(test)]
mod events;
#[cfg(test)]
mod handover;
#[cfg(test)]
mod items;
#[cfg(test)]
mod people;
#[cfg(test)]
mod radar;
#[cfg(test)]
mod ready;
#[cfg(test)]
mod reports;
#[cfg(test)]
mod review;
#[cfg(test)]
mod roles;
#[cfg(test)]
mod sessions;
#[cfg(test)]
mod slots;
#[cfg(test)]
mod stream;
#[cfg(test)]
mod tenancy;
#[cfg(test)]
mod threads;
#[cfg(test)]
mod transport;
#[cfg(test)]
mod ui;
#[cfg(test)]
mod versions;
#[cfg(test)]
mod wide;

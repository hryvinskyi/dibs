//! The client against the real server router, served on an ephemeral port of this machine: every
//! call's typed answer, the refusals it maps, and the failures of a server that cannot be reached,
//! answers too slowly or answers something that is not dibs.

#[cfg(test)]
mod support;

#[cfg(test)]
mod admin;
#[cfg(test)]
mod failures;
#[cfg(test)]
mod items;
#[cfg(test)]
mod threads;

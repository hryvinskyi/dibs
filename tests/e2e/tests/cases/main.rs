//! The built `dibs-server` and `dibs` binaries run as separate processes: concurrent claims, the
//! guard's hooks, the checkouts of a project over several repositories, an integration's import
//! feeding the ready list, a message acknowledged by its recipient, and a wide claim whose edits
//! wait for the acknowledgement of the area's holder.

#[cfg(test)]
mod harness;

#[cfg(test)]
mod acks;
#[cfg(test)]
mod hooks;
#[cfg(test)]
mod race;
#[cfg(test)]
mod ready;
#[cfg(test)]
mod repositories;
#[cfg(test)]
mod wide;

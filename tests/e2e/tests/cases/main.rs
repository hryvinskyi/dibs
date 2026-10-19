//! The built `dibs-server` and `dibs` binaries run as separate processes: concurrent claims, the
//! guard's hooks, the checkouts of a project over several repositories, and an integration's
//! import feeding the ready list.

#[cfg(test)]
mod harness;

#[cfg(test)]
mod hooks;
#[cfg(test)]
mod race;
#[cfg(test)]
mod ready;
#[cfg(test)]
mod repositories;

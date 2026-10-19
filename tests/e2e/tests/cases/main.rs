//! The built `dibs-server` and `dibs` binaries run as separate processes: concurrent claims, the
//! guard's hooks and the checkouts of a project over several repositories.

#[cfg(test)]
mod harness;

#[cfg(test)]
mod hooks;
#[cfg(test)]
mod race;
#[cfg(test)]
mod repositories;

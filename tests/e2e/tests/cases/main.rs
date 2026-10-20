//! The built `dibs-server` and `dibs` binaries run as separate processes: concurrent claims, the
//! guard's hooks, the checkouts of a project over several repositories, an integration's import
//! feeding the ready list, a message acknowledged by its recipient, a wide claim whose edits wait
//! for the acknowledgement of the area's holder, slots taken by racing members and handed to the
//! one waiting, and a shell command refused while another member holds the slot it needs.

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
mod slots;
#[cfg(test)]
mod wide;

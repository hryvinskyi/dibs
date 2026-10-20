//! The built `dibs-server` and `dibs` binaries run as separate processes: concurrent claims, the
//! guard's hooks, the checkouts of a project over several repositories, an integration's import
//! feeding the ready list, a message acknowledged by its recipient, a wide claim whose edits wait
//! for the acknowledgement of the area's holder, slots taken by racing members and handed to the
//! one waiting, a shell command refused while another member holds the slot it needs, a member
//! removed while they held a claim and a slot, a claim through review that an integration's
//! merge report completes, an overlap of two members' branches reported by an integration, a
//! feature claim that receives the area a debt claim hands over, and the figures of a pilot that
//! members' machines reported.

#[cfg(test)]
mod harness;

#[cfg(test)]
mod acks;
#[cfg(test)]
mod handover;
#[cfg(test)]
mod hooks;
#[cfg(test)]
mod people;
#[cfg(test)]
mod pilot;
#[cfg(test)]
mod race;
#[cfg(test)]
mod radar;
#[cfg(test)]
mod ready;
#[cfg(test)]
mod repositories;
#[cfg(test)]
mod review;
#[cfg(test)]
mod slots;
#[cfg(test)]
mod wide;

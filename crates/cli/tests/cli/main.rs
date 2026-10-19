//! The `dibs` binary against the real server router on an ephemeral port: each command's output
//! and exit status, the nearest `.dibs.toml` of a nested checkout, a configuration copied into the
//! wrong clone, a range of commits checked in a real git repository, and a server that cannot be
//! reached. Every variable the binary reads is set on the child process.

#[cfg(test)]
mod support;

#[cfg(test)]
mod admin;
#[cfg(test)]
mod check;
#[cfg(test)]
mod failures;
#[cfg(test)]
mod login;
#[cfg(test)]
mod messages;
#[cfg(test)]
mod work;

//! The Claude Code integration run for real: `dibs hook claude-code <event>` fed synthetic hook
//! input against the in-process server, and the plugin's `dibs-hook` wrapper over fake and real
//! `dibs` binaries.
//!
//! This is a test target of the `dibs` command-line crate, which builds the binary it runs; its
//! source lives beside the adapter, so that only the adapter's directory names the host.

#[cfg(test)]
mod between;
#[cfg(test)]
mod hooks;
#[cfg(test)]
mod support;
#[cfg(test)]
mod wrapper;

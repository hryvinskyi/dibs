//! The server's operations through its binary and its library over real SQLite files: the admin
//! commands and the tokens they print, snapshots and their retention, the retention of records,
//! restores and the hold that keeps them offline, migrations, and stopping on SIGTERM.

#[cfg(test)]
mod support;

#[cfg(test)]
mod admin;
#[cfg(test)]
mod restore;
#[cfg(test)]
mod retention;
#[cfg(test)]
#[cfg(unix)]
mod shutdown;
#[cfg(test)]
mod snapshots;

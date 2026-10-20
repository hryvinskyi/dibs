//! The self-update against a release directory on disk, laid out as the releases are, signed with
//! keys made for each test: what installs, and every way a release is refused with the installed
//! binary left as it was.

#[cfg(test)]
mod cases;

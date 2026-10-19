//! Host-neutral guard for agent hosts.

#[cfg(test)]
mod tests {
    #[test]
    fn crate_identifies_itself() {
        assert_eq!(env!("CARGO_PKG_NAME"), "dibs-agent-host");
    }
}

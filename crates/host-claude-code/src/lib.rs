//! Adapter between one agent host and the neutral guard.

#[cfg(test)]
mod tests {
    #[test]
    fn crate_identifies_itself() {
        assert_eq!(env!("CARGO_PKG_NAME"), "dibs-host-claude-code");
    }
}

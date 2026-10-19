//! HTTP API, web UI and composition root of the server.

#[cfg(test)]
mod tests {
    #[test]
    fn crate_identifies_itself() {
        assert_eq!(env!("CARGO_PKG_NAME"), "dibs-server");
    }
}

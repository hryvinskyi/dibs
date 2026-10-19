//! SQLite adapter for the store port.

#[cfg(test)]
mod tests {
    #[test]
    fn crate_identifies_itself() {
        assert_eq!(env!("CARGO_PKG_NAME"), "dibs-store-sqlite");
    }
}

//! Pure domain model: types, value objects and invariants, free of I/O.

#[cfg(test)]
mod tests {
    #[test]
    fn crate_identifies_itself() {
        assert_eq!(env!("CARGO_PKG_NAME"), "dibs-domain");
    }
}

//! The `dibs` command-line binary and its composition root.

fn main() {}

#[cfg(test)]
mod tests {
    #[test]
    fn crate_identifies_itself() {
        assert_eq!(env!("CARGO_PKG_NAME"), "dibs");
    }
}

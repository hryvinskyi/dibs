//! Rebuilds the crate whenever a migration is added, changed or removed: the migrations are
//! embedded at compile time, and a new file in the directory is otherwise not noticed.

fn main() {
    println!("cargo:rerun-if-changed=migrations");
}

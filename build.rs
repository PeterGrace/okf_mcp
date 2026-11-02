//! Rebuilds the crate when a migration is added or edited: `sqlx::migrate!`
//! embeds `migrations/` at compile time, and cargo would not notice a new file.

fn main() {
    println!("cargo:rerun-if-changed=migrations");
}

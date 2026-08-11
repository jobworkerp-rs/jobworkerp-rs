fn main() {
    println!("cargo:rerun-if-changed=sql/migrations/sqlite");
}

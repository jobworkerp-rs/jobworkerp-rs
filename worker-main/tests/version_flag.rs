//! `<binary> --version` is what the release tooling runs on every built
//! artifact; it must answer before the binary touches `.env`, logging or any
//! connection.

use jobworkerp_base::version_flag::testing::assert_version_flag;
use std::path::Path;

#[test]
fn all_in_one_reports_its_version() {
    assert_version_flag(
        Path::new(env!("CARGO_BIN_EXE_all-in-one")),
        env!("CARGO_PKG_VERSION"),
    );
}

#[test]
fn worker_reports_its_version() {
    assert_version_flag(
        Path::new(env!("CARGO_BIN_EXE_worker")),
        env!("CARGO_PKG_VERSION"),
    );
}

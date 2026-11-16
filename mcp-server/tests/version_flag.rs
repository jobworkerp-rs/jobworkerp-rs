//! `<binary> --version` is what the release tooling runs on every built
//! artifact; it must answer before the binary touches `.env`, logging or any
//! connection.

use jobworkerp_base::version_flag::testing::assert_version_flag;
use std::path::Path;

#[test]
fn mcp_http_reports_its_version() {
    assert_version_flag(
        Path::new(env!("CARGO_BIN_EXE_mcp-http")),
        env!("CARGO_PKG_VERSION"),
    );
}

#[test]
fn mcp_stdio_reports_its_version() {
    assert_version_flag(
        Path::new(env!("CARGO_BIN_EXE_mcp-stdio")),
        env!("CARGO_PKG_VERSION"),
    );
}

//! `--version` handling shared by every distributed binary.
//!
//! Release tooling runs `<binary> --version` on the built artifacts, so the
//! flag must report `<binary file name> <workspace version>` and exit before
//! the binary reads `.env`, initializes logging (which creates log files) or
//! connects to anything.

use std::ffi::OsString;

/// True when the arguments after the program name are exactly `--version`.
/// Any other arguments leave the existing startup path untouched.
pub fn is_version_request<I>(args: I) -> bool
where
    I: IntoIterator<Item = OsString>,
{
    let mut args = args.into_iter();
    args.next().is_some_and(|flag| flag == "--version") && args.next().is_none()
}

/// The line printed for `--version`.
pub fn version_line(binary: &str, version: &str) -> String {
    format!("{binary} {version}")
}

/// Prints the version line and exits when the process was started with only
/// `--version`. Call it first in `main`, before `.env` is loaded or logging is
/// initialized, so the flag has no side effects. A macro so that the binary
/// name and version are those of the calling binary target.
#[macro_export]
macro_rules! exit_if_version_requested {
    () => {
        if $crate::version_flag::is_version_request(::std::env::args_os().skip(1)) {
            println!(
                "{}",
                $crate::version_flag::version_line(
                    env!("CARGO_BIN_NAME"),
                    env!("CARGO_PKG_VERSION")
                )
            );
            ::std::process::exit(0);
        }
    };
}

/// Assertions for the `--version` integration tests of each binary crate.
#[cfg(feature = "test-utils")]
pub mod testing {
    use std::{
        path::Path,
        process::{Command, Stdio},
        time::{Duration, Instant},
    };

    /// A binary that ignores `--version` starts its server instead; give up
    /// on it rather than hang the test run.
    const TIMEOUT: Duration = Duration::from_secs(30);

    /// Runs `binary --version` in an empty directory whose `.env` points
    /// logging at a `logs` subdirectory, and asserts that it prints
    /// `<file name> <version>` and exits without loading `.env` or creating
    /// files.
    pub fn assert_version_flag(binary: &Path, version: &str) {
        let name = binary
            .file_name()
            .and_then(|n| n.to_str())
            .expect("binary file name");
        let workspace = tempfile::tempdir().expect("create temporary workspace");
        let logs = workspace.path().join("logs");
        std::fs::write(
            workspace.path().join(".env"),
            format!("LOG_FILE_DIR={}\nLOG_USE_STDOUT=false\n", logs.display()),
        )
        .expect("write .env");

        let mut child = Command::new(binary)
            .arg("--version")
            .current_dir(workspace.path())
            .env_remove("LOG_FILE_DIR")
            .env_remove("LOG_USE_STDOUT")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap_or_else(|e| panic!("run {}: {e}", binary.display()));
        let started = Instant::now();
        while child.try_wait().expect("wait for the binary").is_none() {
            if started.elapsed() > TIMEOUT {
                let _ = child.kill();
                let _ = child.wait();
                panic!("{name} --version did not exit within {TIMEOUT:?}");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        // Collected after exit: the expected output is a single line, far
        // below the pipe buffer, so the child cannot block on it.
        let output = child.wait_with_output().expect("collect output");

        assert!(
            output.status.success(),
            "{name} --version failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            format!("{}\n", super::version_line(name, version))
        );
        let entries: Vec<_> = std::fs::read_dir(workspace.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(
            entries,
            [".env"],
            "{name} --version created files (logging initialized?)"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    #[test]
    fn only_a_lone_version_flag_is_a_version_request() {
        assert!(is_version_request(args(&["--version"])));
        assert!(!is_version_request(args(&[])));
        assert!(!is_version_request(args(&["--version", "extra"])));
        assert!(!is_version_request(args(&["extra", "--version"])));
        assert!(!is_version_request(args(&["-V"])));
        assert!(!is_version_request(args(&["--versions"])));
    }

    #[test]
    fn version_line_is_name_and_version() {
        assert_eq!(version_line("worker", "1.2.3"), "worker 1.2.3");
    }
}

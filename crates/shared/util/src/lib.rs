//! Shared infrastructure utilities.
//!
//! Small helpers that involve OS I/O (filesystem, environment) but don't
//! belong in the pure `domain` crate.

pub mod paths;

/// Find a binary in the system PATH.
///
/// Handles platform-correct path separators (`:` on Unix, `;` on Windows).
/// Returns the absolute path to the first matching executable, or `None`.
pub fn which(name: &str) -> Option<String> {
    let path_var = std::env::var("PATH").ok()?;

    #[cfg(windows)]
    let separator = ';';
    #[cfg(not(windows))]
    let separator = ':';

    for dir in path_var.split(separator) {
        let candidate = std::path::Path::new(dir).join(name);
        if candidate.exists() {
            return candidate.to_str().map(|s| s.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn which_finds_common_binary() {
        #[cfg(windows)]
        assert!(which("cmd.exe").is_some());
        #[cfg(not(windows))]
        assert!(which("sh").is_some());
    }

    #[test]
    fn which_returns_none_for_missing() {
        assert!(which("nonexistent-binary-xyz-12345").is_none());
    }
}

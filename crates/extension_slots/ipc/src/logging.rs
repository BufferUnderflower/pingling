use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone)]
pub struct LogFiles {
    pub directory: PathBuf,
    pub session_file: PathBuf,
    pub latest_file: PathBuf,
}

pub fn init(service_name: &str) -> io::Result<LogFiles> {
    let log_files = resolve_log_files(service_name);
    fs::create_dir_all(&log_files.directory)?;
    if let Some(parent) = log_files.session_file.parent() {
        fs::create_dir_all(parent)?;
    }

    let session = open_append(&log_files.session_file)?;
    let latest = open_truncate(&log_files.latest_file)?;

    unsafe {
        std::env::set_var("PINGLING_DAEMON_LOG_FILE", &log_files.latest_file);
    }

    let env = env_logger::Env::default().default_filter_or("ipc_server=debug,info");
    let mut builder = env_logger::Builder::from_env(env);
    builder.target(env_logger::Target::Pipe(Box::new(TeeWriter {
        stderr: io::stderr(),
        session,
        latest,
    })));
    builder.format_timestamp_millis();
    builder.init();

    Ok(log_files)
}

fn resolve_log_files(service_name: &str) -> LogFiles {
    let latest_file = pingling_util::paths::log_file();
    let directory = latest_file
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| std::env::temp_dir().join("pingling"));
    let session_file = directory.join("logs").join(format!(
        "{}-{}-pid{}.log",
        service_name,
        unix_timestamp(),
        std::process::id()
    ));
    LogFiles {
        directory,
        session_file,
        latest_file,
    }
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn open_append(path: &Path) -> io::Result<File> {
    OpenOptions::new().create(true).append(true).open(path)
}

fn open_truncate(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
}

struct TeeWriter {
    stderr: io::Stderr,
    session: File,
    latest: File,
}

impl Write for TeeWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut file_error: Option<io::Error> = None;
        let _ = self.stderr.write_all(buf);
        if let Err(error) = self.session.write_all(buf) {
            file_error.get_or_insert(error);
        }
        if let Err(error) = self.latest.write_all(buf) {
            file_error.get_or_insert(error);
        }
        match file_error {
            Some(error) => Err(error),
            None => Ok(buf.len()),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        let _ = self.stderr.flush();
        self.session.flush()?;
        self.latest.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use tempfile::TempDir;

    struct EnvGuard {
        key: &'static str,
        previous: Option<std::ffi::OsString>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &std::path::Path) -> Self {
            let previous = std::env::var_os(key);
            unsafe { std::env::set_var(key, value) };
            Self { key, previous }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match self.previous.as_ref() {
                Some(value) => unsafe { std::env::set_var(self.key, value) },
                None => unsafe { std::env::remove_var(self.key) },
            }
        }
    }

    fn install_runtime_env(root: &std::path::Path) -> Vec<EnvGuard> {
        let mut guards = Vec::new();
        for key in [
            "HOME",
            "XDG_CONFIG_HOME",
            "XDG_CACHE_HOME",
            "APPDATA",
            "LOCALAPPDATA",
            "TMPDIR",
            "TEMP",
            "TMP",
        ] {
            guards.push(EnvGuard::set(key, root));
        }
        guards
    }

    #[test]
    #[serial]
    fn resolve_log_files_uses_runtime_cache_root() {
        let root = TempDir::new().expect("runtime tempdir");
        let _guards = install_runtime_env(root.path());
        let log_files = resolve_log_files("ipc-server-headless");
        assert!(log_files.latest_file.ends_with("daemon.log"));
        assert!(log_files
            .session_file
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.contains("ipc-server-headless")));
        assert_eq!(log_files.directory, pingling_util::paths::cache_root());
    }
}

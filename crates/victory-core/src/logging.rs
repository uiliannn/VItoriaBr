//! One tracing setup for every binary.
//!
//! Filter with `VICTORY_LOG` (falls back to `RUST_LOG`), e.g.
//! `VICTORY_LOG=victory_loader=trace,info`. Debug and trace output exist
//! only in builds with the `diagnostics` feature; a plain release build
//! logs info and above whatever the filter asks for.
//! An optional directory also receives append-only plain text in `victory.log`.

use std::fs::OpenOptions;
use std::path::Path;
use std::sync::Mutex;

use anyhow::{Context, Result};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// The most verbose level this build can log.
const CAP: LevelFilter = if cfg!(feature = "diagnostics") {
    LevelFilter::TRACE
} else {
    LevelFilter::INFO
};

pub fn init(default_level: &str, log_dir: Option<&Path>) -> Result<()> {
    subscriber(default_level, log_dir)?
        .try_init()
        .context("initializing logging")
}

fn subscriber(
    default_level: &str,
    log_dir: Option<&Path>,
) -> Result<impl tracing::Subscriber + Send + Sync> {
    let file = log_dir
        .map(|dir| {
            let path = dir.join("victory.log");
            crate::ensure_outside_repo(&path)?;
            std::fs::create_dir_all(dir)
                .with_context(|| format!("creating log directory {}", dir.display()))?;
            let file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .with_context(|| format!("opening log file {}", path.display()))?;
            Ok::<_, anyhow::Error>(
                tracing_subscriber::fmt::layer()
                    .with_target(true)
                    .with_ansi(false)
                    .with_writer(Mutex::new(file)),
            )
        })
        .transpose()?;
    let filter = std::env::var("VICTORY_LOG")
        .or_else(|_| std::env::var("RUST_LOG"))
        .ok()
        .and_then(|v| EnvFilter::try_new(v).ok())
        .unwrap_or_else(|| EnvFilter::new(default_level));
    Ok(tracing_subscriber::registry()
        .with(CAP)
        .with(filter)
        .with(
            tracing_subscriber::fmt::layer()
                .with_target(true)
                .with_writer(std::io::stderr),
        )
        .with(file))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_logging_creates_directories_and_appends_plain_text() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("logs");
        for message in ["first session", "second session"] {
            let subscriber = subscriber("info", Some(&dir)).unwrap();
            tracing::subscriber::with_default(subscriber, || {
                tracing::info!(message, "file logging test");
            });
        }
        let text = std::fs::read_to_string(dir.join("victory.log")).unwrap();
        assert!(text.contains("first session"));
        assert!(text.contains("second session"));
        assert!(!text.contains('\x1b'));
    }

    #[test]
    fn file_logging_reports_invalid_destinations() {
        let inside = Path::new(env!("CARGO_MANIFEST_DIR")).join("logs");
        assert!(subscriber("info", Some(&inside)).is_err());
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("not-a-directory");
        std::fs::write(&file, "").unwrap();
        assert!(subscriber("info", Some(&file)).is_err());
    }
}

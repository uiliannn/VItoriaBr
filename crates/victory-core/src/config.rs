//! Where the user's files live.
//!
//! Resolution order for each value: environment variable, then the config
//! file (`VICTORY_CONFIG`, else `./victory.toml`), then the default.
//! See `victory.example.toml` at the repository root.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::policy::{ensure_outside_repo, is_within};

#[derive(Debug, Default, Clone)]
pub struct Config {
    /// The user's dump of the title.
    pub game_dir: Option<PathBuf>,
    /// Root for all derived data. Always outside the repository.
    pub cache_dir: PathBuf,
    /// What titles write and must keep (persistent local storage). Never
    /// inside the cache, which may be deleted at any time.
    pub data_dir: PathBuf,
    /// Other titles used to exercise platform-level code.
    pub test_titles: Vec<PathBuf>,
    /// The config file that was read, if any.
    pub source: Option<PathBuf>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    #[serde(default)]
    paths: FilePaths,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FilePaths {
    game_dir: Option<PathBuf>,
    cache_dir: Option<PathBuf>,
    data_dir: Option<PathBuf>,
    #[serde(default)]
    test_titles: Vec<PathBuf>,
}

impl Config {
    /// Loads configuration from the environment and the config file.
    pub fn load() -> Result<Self> {
        let explicit = std::env::var_os("VICTORY_CONFIG").map(PathBuf::from);
        let path = explicit
            .clone()
            .unwrap_or_else(|| PathBuf::from("victory.toml"));
        let file = if path.is_file() {
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            Some((path, text))
        } else if let Some(p) = explicit {
            anyhow::bail!(
                "VICTORY_CONFIG points at {}, which does not exist",
                p.display()
            );
        } else {
            None
        };
        let (source, parsed) = match file {
            Some((path, text)) => {
                let parsed: FileConfig =
                    toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
                (Some(path), parsed)
            }
            None => (None, FileConfig::default()),
        };
        Self::resolve(parsed.paths, source, |k| std::env::var_os(k))
    }

    fn resolve(
        file: FilePaths,
        source: Option<PathBuf>,
        env: impl Fn(&str) -> Option<std::ffi::OsString>,
    ) -> Result<Self> {
        let env_path = |k: &str| env(k).filter(|v| !v.is_empty()).map(PathBuf::from);
        let test_titles = match env("VICTORY_TEST_TITLES") {
            Some(v) if !v.is_empty() => std::env::split_paths(&v)
                .filter(|p| !p.as_os_str().is_empty())
                .collect(),
            _ => file.test_titles,
        };
        let cache_dir = env_path("VICTORY_CACHE_DIR")
            .or(file.cache_dir)
            .map_or_else(|| default_dir("cache"), Ok)?;
        let data_dir = env_path("VICTORY_DATA_DIR")
            .or(file.data_dir)
            .map_or_else(|| default_dir("data"), Ok)?;
        if is_within(&data_dir, &cache_dir)? {
            anyhow::bail!(
                "data_dir ({}) must not be the cache_dir ({}) or inside it: the cache may be \
                 deleted at any time",
                data_dir.display(),
                cache_dir.display()
            );
        }
        Ok(Config {
            game_dir: env_path("VICTORY_GAME_DIR").or(file.game_dir),
            cache_dir,
            data_dir,
            test_titles,
            source,
        })
    }

    /// The game directory, or a helpful error if it is not configured.
    pub fn require_game_dir(&self) -> Result<&Path> {
        self.game_dir.as_deref().context(
            "no game directory configured: set VICTORY_GAME_DIR or [paths].game_dir in \
             victory.toml (see victory.example.toml)",
        )
    }

    /// Returns `<cache>/<sub>`, creating it, after checking the asset policy.
    pub fn cache_subdir(&self, sub: &str) -> Result<PathBuf> {
        subdir(&self.cache_dir, sub)
    }

    /// Returns `<data>/<sub>`, creating it, after checking the asset policy.
    pub fn data_subdir(&self, sub: &str) -> Result<PathBuf> {
        subdir(&self.data_dir, sub)
    }
}

fn subdir(root: &Path, sub: &str) -> Result<PathBuf> {
    let dir = root.join(sub);
    ensure_outside_repo(&dir)?;
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    Ok(dir)
}

/// `%LOCALAPPDATA%/victory/<name>` (elsewhere the XDG or home cache).
fn default_dir(name: &str) -> Result<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")
        .or_else(|| std::env::var_os("XDG_CACHE_HOME"))
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .context("cannot determine a user directory; set VICTORY_CACHE_DIR and VICTORY_DATA_DIR")?;
    Ok(base.join("victory").join(name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    #[test]
    fn environment_overrides_file() {
        let file = FilePaths {
            game_dir: Some("from-file".into()),
            cache_dir: Some("cache-from-file".into()),
            ..Default::default()
        };
        let env = |k: &str| (k == "VICTORY_GAME_DIR").then(|| OsString::from("from-env"));
        let cfg = Config::resolve(file, None, env).unwrap();
        assert_eq!(cfg.game_dir.unwrap(), PathBuf::from("from-env"));
        assert_eq!(cfg.cache_dir, PathBuf::from("cache-from-file"));
    }

    #[test]
    fn empty_env_values_are_ignored() {
        let file = FilePaths {
            game_dir: Some("from-file".into()),
            ..Default::default()
        };
        let env = |k: &str| (k == "VICTORY_GAME_DIR").then(OsString::new);
        let cfg = Config::resolve(file, None, env).unwrap();
        assert_eq!(cfg.game_dir.unwrap(), PathBuf::from("from-file"));
    }

    #[test]
    fn empty_test_titles_env_is_ignored() {
        let file = FilePaths {
            test_titles: vec!["from-file".into()],
            ..Default::default()
        };
        let env = |k: &str| (k == "VICTORY_TEST_TITLES").then(OsString::new);
        let cfg = Config::resolve(file, None, env).unwrap();
        assert_eq!(cfg.test_titles, vec![PathBuf::from("from-file")]);
    }

    #[test]
    fn test_titles_env_splits_paths() {
        let value = std::env::join_paths(["first", "second"]).unwrap();
        let file = FilePaths::default();
        let env = |k: &str| (k == "VICTORY_TEST_TITLES").then(|| value.clone());
        let cfg = Config::resolve(file, None, env).unwrap();
        assert_eq!(
            cfg.test_titles,
            vec![PathBuf::from("first"), PathBuf::from("second")]
        );
    }

    #[test]
    fn example_config_parses() {
        let text = include_str!("../../../victory.example.toml");
        let parsed: FileConfig = toml::from_str(text).unwrap();
        assert!(parsed.paths.game_dir.is_some());
    }

    #[test]
    fn cache_inside_repo_is_refused() {
        let inside = Path::new(env!("CARGO_MANIFEST_DIR")).join("cache");
        let cfg = Config {
            cache_dir: inside,
            ..Default::default()
        };
        assert!(cfg.cache_subdir("x").is_err());
    }

    #[test]
    fn data_inside_repo_is_refused() {
        let inside = Path::new(env!("CARGO_MANIFEST_DIR")).join("data");
        let cfg = Config {
            data_dir: inside,
            ..Default::default()
        };
        assert!(cfg.data_subdir("x").is_err());
    }

    fn resolve_dirs(cache: &Path, data: &Path) -> Result<Config> {
        let file = FilePaths {
            cache_dir: Some(cache.to_path_buf()),
            data_dir: Some(data.to_path_buf()),
            ..Default::default()
        };
        Config::resolve(file, None, |_| None)
    }

    #[test]
    fn data_dir_equal_to_cache_dir_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(resolve_dirs(tmp.path(), tmp.path()).is_err());
    }

    #[test]
    fn data_dir_inside_cache_dir_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("cache");
        assert!(resolve_dirs(&cache, &cache.join("data")).is_err());
        assert!(resolve_dirs(&cache, &cache.join("a/../data")).is_err());
    }

    #[test]
    fn sibling_with_similar_prefix_and_cache_inside_data_are_allowed() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("cache");
        assert!(resolve_dirs(&cache, &tmp.path().join("cache-old")).is_ok());
        let data = tmp.path().join("data");
        assert!(resolve_dirs(&data.join("cache"), &data).is_ok());
    }

    #[cfg(windows)]
    mod missing_windows_dirs {
        use super::*;
        use std::fs;

        fn temp_with_case_probe(name: &str, alias: &str, expect_alias: bool) -> tempfile::TempDir {
            let tmp = tempfile::tempdir().unwrap();
            let parent = tmp.path();
            assert!(parent.is_absolute() && parent.is_dir());
            let probe = parent.join(name);
            fs::create_dir(&probe).unwrap();
            assert_eq!(
                parent.join(alias).try_exists().unwrap(),
                expect_alias,
                "unexpected case behavior for {name} and {alias} in {}",
                parent.display()
            );
            fs::remove_dir(probe).unwrap();
            tmp
        }

        fn check_missing_dirs(
            parent: &Path,
            cache_name: &str,
            data_name: &str,
            expect_overlap: bool,
        ) {
            let cache = parent.join(cache_name);
            let data = parent.join(data_name);
            assert!(!cache.try_exists().unwrap() && !data.try_exists().unwrap());
            assert!(fs::read_dir(parent).unwrap().next().is_none());

            let result = resolve_dirs(&cache, &data);
            // Check before inspecting the result, including when an expected rejection is missing.
            assert!(
                fs::read_dir(parent).unwrap().next().is_none(),
                "configuration created an entry in {}",
                parent.display()
            );
            if expect_overlap {
                let error = result.expect_err("accepted overlapping cache and data directories");
                let diagnostic = format!(
                    "data_dir ({}) must not be the cache_dir ({}) or inside it",
                    data.display(),
                    cache.display()
                );
                assert!(error.to_string().starts_with(&diagnostic), "{error}");
            } else {
                result.unwrap();
            }
        }

        #[test]
        fn rejects_equal_dirs_with_different_case() {
            let tmp = temp_with_case_probe("case-probe", "CASE-PROBE", true);
            check_missing_dirs(tmp.path(), "cache", "CACHE", true);
        }

        #[test]
        fn rejects_descendant_with_different_case() {
            let tmp = temp_with_case_probe("case-probe", "CASE-PROBE", true);
            check_missing_dirs(tmp.path(), "cache", "CACHE/saves", true);
        }

        #[test]
        fn rejects_descendant_with_equivalent_unicode_case() {
            let tmp = temp_with_case_probe("probe-depósito", "PROBE-DEPÓSITO", true);
            check_missing_dirs(tmp.path(), "depósito", "DEPÓSITO/saves", true);
        }

        #[test]
        fn accepts_sibling_with_similar_name() {
            let tmp = temp_with_case_probe("case-probe", "CASE-PROBE", true);
            check_missing_dirs(tmp.path(), "cache", "CACHE-old/saves", false);
        }

        #[test]
        fn rejects_same_case_descendant_without_creating_dirs() {
            let tmp = temp_with_case_probe("case-probe", "CASE-PROBE", true);
            check_missing_dirs(tmp.path(), "cache", "cache/saves", true);
        }

        #[test]
        #[ignore = "requires a case-sensitive Windows temporary directory"]
        fn respects_case_sensitive_parent() {
            let tmp = temp_with_case_probe("case-probe", "CASE-PROBE", false);
            check_missing_dirs(tmp.path(), "cache", "CACHE/saves", false);
            check_missing_dirs(tmp.path(), "cache", "cache/saves", true);
        }
    }
}

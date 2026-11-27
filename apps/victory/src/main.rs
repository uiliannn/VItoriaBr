//! `victory`: the command-line front end.
//!
//! Every command reads the user's own files and writes only to stdout or to
//! the user's cache directory (outside the repository).

mod census;
mod inspect;
mod run;
mod shader;
mod translate;
mod warm;

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser)]
#[command(
    name = "VItoriaBr",
    version,
    about = "Run an Xbox Series title natively on Windows"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Summarise one PE image: headers, sections, imports, TLS, build id.
    Inspect {
        file: PathBuf,
        /// List every imported function.
        #[arg(long)]
        imports: bool,
        /// List every exported function.
        #[arg(long)]
        exports: bool,
    },
    /// Aggregate the imports of every PE image under the given paths and
    /// show which ones our runtime covers. Defaults to the configured
    /// game and test-title directories.
    Census {
        paths: Vec<PathBuf>,
        /// Do not count host pass-through as covered.
        #[arg(long)]
        strict: bool,
        /// List every missing `dll!function`, most used first.
        #[arg(long)]
        missing: bool,
    },
    /// List the `dll!export` pairs an import library describes.
    LibExports {
        lib: PathBuf,
        /// List every export, not just per-DLL counts.
        #[arg(long)]
        list: bool,
    },
    /// Find shader containers in files or directories (also inside engine
    /// archives, if uncompressed) and report their layout.
    ShaderInfo {
        paths: Vec<PathBuf>,
        /// Print one line per container found.
        #[arg(long)]
        verbose: bool,
        /// Also list the decoded native instructions (implies --verbose).
        #[arg(long)]
        disasm: bool,
    },
    /// Run every shader found (containers, SGD5 files) through the
    /// translator and report what it cannot handle yet.
    Translate {
        paths: Vec<PathBuf>,
        /// Check emitted DXIL with dxil.dll's validator (`VICTORY_DXIL`,
        /// else the newest Windows SDK's).
        #[arg(long)]
        validate: bool,
        /// Also create a host D3D12 pipeline for each emitted shader.
        #[arg(long)]
        pipelines: bool,
        /// Print one line per program and outcome.
        #[arg(long)]
        list: bool,
        /// Worker threads (default: every logical core).
        #[arg(long)]
        jobs: Option<usize>,
        /// Warm the run's shader cache: a file of shader hashes (the 16
        /// hex digits a run's log names a shader by, anywhere in a line);
        /// the containers with those hashes are translated into the
        /// cache and nothing else is done. Set `VICTORY_HOST_WAVE` and
        /// `VICTORY_VIEW_LIMIT` to what the run's log says of the host.
        #[arg(long)]
        warm: Option<PathBuf>,
    },
    /// Translate every shader found into the shader cache ahead of a run,
    /// so the first run reads them from the cache instead of translating
    /// them while the title waits. Run it with the same build and
    /// VICTORY_* settings as the title (including `VICTORY_HOST_WAVE` and
    /// `VICTORY_VIEW_LIMIT` when matching the host run's log). Defaults to
    /// the configured game and test-title directories.
    Warm {
        paths: Vec<PathBuf>,
        /// Print each shader that does not translate.
        #[arg(long)]
        list: bool,
        /// Worker threads (default: every logical core).
        #[arg(long)]
        jobs: Option<usize>,
    },
    /// Prepare, map and bind a title executable; with --execute, run it.
    Run {
        exe: PathBuf,
        /// What unimplemented imports do when called.
        #[arg(long, value_enum, default_value_t = Unimplemented::Trap)]
        unimplemented: Unimplemented,
        /// Disable host pass-through (everything must be implemented by us).
        #[arg(long)]
        strict: bool,
        /// Actually start the title. Without it, stop after binding.
        #[arg(long)]
        execute: bool,
        /// The console model the title sees (device type, memory).
        #[arg(long, value_enum, default_value_t = Console::SeriesX)]
        console: Console,
        /// Arguments for the title, after `--` (it reads the whole command
        /// line; a title that relaunches itself passes some).
        #[arg(last = true)]
        title_args: Vec<String>,
    },
    /// Show the resolved configuration and check it.
    Config,
}

#[derive(Clone, Copy, ValueEnum)]
enum Unimplemented {
    Trap,
    Zero,
}

#[derive(Clone, Copy, ValueEnum)]
enum Console {
    SeriesX,
    SeriesS,
}

fn main() -> Result<()> {
    let preset = apply_preset_from_env();
    victory_core::logging::init("info");
    match preset {
        PresetOutcome::Unset => {}
        PresetOutcome::Applied { name, count } => {
            eprintln!("victory: applied VICTORY_PRESET={name} ({count} defaults)");
        }
        PresetOutcome::Unknown(value) => {
            eprintln!("victory: warning: unknown VICTORY_PRESET={value:?}; ignoring");
        }
    }
    // The audio recorder `run` starts for `VICTORY_AUDIO_CAPTURE`.
    #[cfg(all(windows, feature = "diagnostics"))]
    if victory_hle::modules::audio::capture::child() {
        return Ok(());
    }
    let cli = Cli::parse();
    match cli.command {
        Command::Inspect {
            file,
            imports,
            exports,
        } => inspect::run(&file, imports, exports),
        Command::Census {
            paths,
            strict,
            missing,
        } => census::run(paths, strict, missing),
        Command::LibExports { lib, list } => inspect::lib_exports(&lib, list),
        Command::ShaderInfo {
            paths,
            verbose,
            disasm,
        } => shader::run(paths, verbose || disasm, disasm),
        Command::Translate {
            paths,
            validate,
            pipelines,
            list,
            jobs,
            warm,
        } => {
            apply_host_shader_overrides_from_env();
            if let Some(file) = warm {
                return translate::warm(paths, &file, jobs);
            }
            translate::run(paths, validate, pipelines, list, jobs)
        }
        Command::Warm { paths, list, jobs } => {
            apply_host_shader_overrides_from_env();
            warm::run(paths, list, jobs)
        }
        Command::Run {
            exe,
            unimplemented,
            strict,
            execute,
            console,
            title_args: _,
        } => run::run(&exe, unimplemented, strict, execute, console),
        Command::Config => show_config(),
    }
}

fn show_config() -> Result<()> {
    let cfg = victory_core::Config::load()?;
    let status = |p: &Option<PathBuf>| match p {
        Some(p) if p.exists() => format!("{}", p.display()),
        Some(p) => format!("{}  (does not exist)", p.display()),
        None => "(not set)".to_owned(),
    };
    println!(
        "config file : {}",
        cfg.source
            .as_ref()
            .map_or("(none)".into(), |p| p.display().to_string())
    );
    println!("game_dir    : {}", status(&cfg.game_dir));
    println!("cache_dir   : {}", cfg.cache_dir.display());
    println!("data_dir    : {}", cfg.data_dir.display());
    for t in &cfg.test_titles {
        println!("test_title  : {}", status(&Some(t.clone())));
    }
    for p in cfg
        .game_dir
        .iter()
        .chain(&cfg.test_titles)
        .chain([&cfg.cache_dir, &cfg.data_dir])
    {
        if let Err(e) = victory_core::ensure_outside_repo(p) {
            println!("POLICY VIOLATION: {e}");
        }
    }
    Ok(())
}

/// Paths given on the command line, or the configured game + test titles.
pub(crate) fn default_paths(paths: Vec<PathBuf>) -> Result<Vec<PathBuf>> {
    if !paths.is_empty() {
        return Ok(paths);
    }
    let cfg = victory_core::Config::load()?;
    let all: Vec<PathBuf> = cfg.game_dir.into_iter().chain(cfg.test_titles).collect();
    anyhow::ensure!(
        !all.is_empty(),
        "no paths given and none configured (see victory.example.toml)"
    );
    Ok(all)
}

/// Default environment overrides for `VICTORY_PRESET=8gb` (8 GB VRAM GPUs
/// such as Radeon RX 6500 XT over PCIe 3.0 x4, with title files on an HDD).
const PRESET_8GB: &[(&str, &str)] = &[
    ("VICTORY_RELAY", "0"),
    ("VICTORY_FILL_WAIT", "1"),
    ("VICTORY_CARD_BUFFERS_MB", "1024"),
    ("VICTORY_TEXTURE_BUDGET_MB", "5120"),
    ("VICTORY_PAGE_WRITES", "slots"),
    ("VICTORY_AMBIENT", "0.6"),
    ("VICTORY_TEXTURE_NO_WAIT", "1"),
    ("VICTORY_WIDE_REFRESH", "1"),
    ("VICTORY_WIDE_MERGE", "1"),
    ("VICTORY_JOIN_COPIES", "1"),
    ("VICTORY_DISPLAY_MODE", "1920x1080"),
];

#[derive(Debug, PartialEq, Eq)]
enum PresetOutcome {
    Unset,
    Applied { name: &'static str, count: usize },
    Unknown(String),
}

/// Applies `VICTORY_PRESET` defaults using the supplied lookup/setter hooks
/// without overwriting variables already set by the user.
fn apply_preset_with(
    preset: Option<&str>,
    mut is_set: impl FnMut(&str) -> bool,
    mut set: impl FnMut(&str, &str),
) -> PresetOutcome {
    let Some(raw) = preset.map(str::trim).filter(|s| !s.is_empty()) else {
        return PresetOutcome::Unset;
    };
    let entries: &[(&str, &str)] = match raw.to_ascii_lowercase().as_str() {
        "8gb" => PRESET_8GB,
        _ => return PresetOutcome::Unknown(raw.to_owned()),
    };
    let mut count = 0;
    for &(key, value) in entries {
        if !is_set(key) {
            set(key, value);
            count += 1;
        }
    }
    PresetOutcome::Applied { name: "8gb", count }
}

fn apply_preset_from_env() -> PresetOutcome {
    let preset = std::env::var("VICTORY_PRESET").ok();
    apply_preset_with(
        preset.as_deref(),
        |key| std::env::var_os(key).is_some_and(|v| !v.is_empty()),
        |key, value| {
            // SAFETY: invoked at the very start of `main` before any worker
            // threads are spawned or other modules read `VICTORY_*`.
            unsafe { std::env::set_var(key, value) };
        },
    )
}

/// Applies offline `VICTORY_VIEW_LIMIT` and `VICTORY_HOST_WAVE` overrides so
/// `victory translate` and `victory warm` produce cache keys matching the
/// runtime device's view-heap cap and wave width.
fn apply_host_shader_overrides(view_limit: Option<&str>, host_wave: Option<&str>) {
    if let Some(n) = view_limit.and_then(|v| v.trim().parse().ok()) {
        victory_shader::emit::set_view_heap_limit(n);
    }
    if let Some(n) = host_wave.and_then(|v| v.trim().parse().ok()) {
        victory_shader::emit::set_host_max_wave(n);
    }
}

fn apply_host_shader_overrides_from_env() {
    apply_host_shader_overrides(
        std::env::var("VICTORY_VIEW_LIMIT").ok().as_deref(),
        std::env::var("VICTORY_HOST_WAVE").ok().as_deref(),
    );
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{PRESET_8GB, PresetOutcome, apply_host_shader_overrides, apply_preset_with};
    use victory_shader::emit::{
        host_max_wave, set_host_max_wave, set_view_heap_limit, translation_fingerprint,
        view_heap_limit,
    };

    #[test]
    fn offline_host_overrides_update_shader_cache_fingerprint() {
        let prev_limit = view_heap_limit();
        let prev_wave = host_max_wave();

        set_view_heap_limit(0);
        set_host_max_wave(0);
        let default_fp = translation_fingerprint();

        apply_host_shader_overrides(Some(" 999999 "), Some("32"));
        assert_eq!(view_heap_limit(), 999_999);
        assert_eq!(host_max_wave(), 32);
        let tuned_fp = translation_fingerprint();
        assert_ne!(default_fp, tuned_fp);

        // Invalid values keep existing settings untouched.
        apply_host_shader_overrides(Some("bad"), None);
        assert_eq!(view_heap_limit(), 999_999);
        assert_eq!(host_max_wave(), 32);

        set_view_heap_limit(prev_limit);
        set_host_max_wave(prev_wave);
    }

    #[test]
    fn preset_8gb_sets_all_defaults_when_unset() {
        let env: std::cell::RefCell<HashMap<String, String>> =
            std::cell::RefCell::new(HashMap::new());
        let outcome = apply_preset_with(
            Some("8gb"),
            |k| env.borrow().get(k).is_some_and(|v| !v.is_empty()),
            |k, v| {
                env.borrow_mut().insert(k.to_owned(), v.to_owned());
            },
        );
        let env = env.into_inner();
        assert_eq!(
            outcome,
            PresetOutcome::Applied {
                name: "8gb",
                count: PRESET_8GB.len(),
            }
        );
        assert_eq!(env.get("VICTORY_RELAY").map(String::as_str), Some("0"));
        assert_eq!(env.get("VICTORY_FILL_WAIT").map(String::as_str), Some("1"));
        assert_eq!(
            env.get("VICTORY_CARD_BUFFERS_MB").map(String::as_str),
            Some("1024")
        );
        assert_eq!(
            env.get("VICTORY_TEXTURE_BUDGET_MB").map(String::as_str),
            Some("5120")
        );
        assert_eq!(
            env.get("VICTORY_PAGE_WRITES").map(String::as_str),
            Some("slots")
        );
        assert_eq!(env.get("VICTORY_AMBIENT").map(String::as_str), Some("0.6"));
        assert_eq!(
            env.get("VICTORY_TEXTURE_NO_WAIT").map(String::as_str),
            Some("1")
        );
        assert_eq!(
            env.get("VICTORY_WIDE_REFRESH").map(String::as_str),
            Some("1")
        );
        assert_eq!(
            env.get("VICTORY_WIDE_MERGE").map(String::as_str),
            Some("1")
        );
        assert_eq!(
            env.get("VICTORY_JOIN_COPIES").map(String::as_str),
            Some("1")
        );
        assert_eq!(
            env.get("VICTORY_DISPLAY_MODE").map(String::as_str),
            Some("1920x1080")
        );
    }

    #[test]
    fn preset_8gb_preserves_explicit_user_env_vars() {
        let env = std::cell::RefCell::new(HashMap::from([
            ("VICTORY_RELAY".to_owned(), "2".to_owned()),
            ("VICTORY_CARD_BUFFERS_MB".to_owned(), "2048".to_owned()),
            ("VICTORY_DISPLAY_MODE".to_owned(), "2560x1440".to_owned()),
        ]));
        let outcome = apply_preset_with(
            Some(" 8GB "),
            |k| env.borrow().get(k).is_some_and(|v| !v.is_empty()),
            |k, v| {
                env.borrow_mut().insert(k.to_owned(), v.to_owned());
            },
        );
        let env = env.into_inner();
        assert_eq!(
            outcome,
            PresetOutcome::Applied {
                name: "8gb",
                count: PRESET_8GB.len() - 3,
            }
        );
        assert_eq!(env.get("VICTORY_RELAY").map(String::as_str), Some("2"));
        assert_eq!(
            env.get("VICTORY_CARD_BUFFERS_MB").map(String::as_str),
            Some("2048")
        );
        assert_eq!(
            env.get("VICTORY_DISPLAY_MODE").map(String::as_str),
            Some("2560x1440")
        );
        assert_eq!(
            env.get("VICTORY_TEXTURE_BUDGET_MB").map(String::as_str),
            Some("5120")
        );
    }

    #[test]
    fn preset_unset_empty_or_unknown_does_not_modify_env() {
        for (input, expected) in [
            (None, PresetOutcome::Unset),
            (Some(""), PresetOutcome::Unset),
            (Some("   "), PresetOutcome::Unset),
            (Some("unknown"), PresetOutcome::Unknown("unknown".to_owned())),
        ] {
            let env: std::cell::RefCell<HashMap<String, String>> =
                std::cell::RefCell::new(HashMap::new());
            let outcome = apply_preset_with(
                input,
                |k| env.borrow().get(k).is_some_and(|v| !v.is_empty()),
                |k, v| {
                    env.borrow_mut().insert(k.to_owned(), v.to_owned());
                },
            );
            assert_eq!(outcome, expected);
            assert!(env.borrow().is_empty());
        }
    }
}

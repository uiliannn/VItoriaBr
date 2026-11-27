//! D3D12.X on host D3D12.
//!
//! D3D12.X records commands from code inlined into the title, which writes
//! packets into the command list while its put pointer is below a limit and
//! calls the list's method otherwise. Our lists keep the limits at zero, so
//! every command arrives here as a call, to be run on host D3D12. The
//! title's GPU addresses are CPU addresses (console memory is unified).
//!
//! [`device`] is the device object titles create, [`dxgi`] its adapter side,
//! [`resource`] heaps and resources, [`fence`] fences, [`queue`] command
//! queues, [`list`] command allocators and lists, [`descriptor`]
//! descriptor heaps, [`root`] root signatures, [`query`] query heaps,
//! [`signature`] command signatures, [`pipeline`] pipeline states,
//! [`frame`] frame pacing, [`object`] what all console graphics objects
//! share, [`console`] which console model the title sees. Planned:
//! `present` (swap chain).

#[cfg(windows)]
mod ambient;
#[cfg(all(windows, feature = "diagnostics"))]
mod capture;
#[cfg(windows)]
mod cardmeter;
#[cfg(all(windows, feature = "diagnostics"))]
mod renderdoc;
#[cfg(all(windows, feature = "diagnostics"))]
mod shot;
#[cfg(windows)]
mod compose;
pub mod console;
#[cfg(all(windows, feature = "diagnostics"))]
mod copystat;
pub mod descriptor;
pub mod device;
#[cfg(windows)]
mod drvwatch;
#[cfg(windows)]
mod fpsbox;
#[cfg(windows)]
mod spread;
#[cfg(windows)]
mod draw;
pub mod dxgi;
pub mod fence;
pub mod frame;
#[cfg(windows)]
pub mod framelog;
#[cfg(windows)]
mod host;
#[cfg(windows)]
mod indirect;
#[cfg(windows)]
mod inplace;
pub mod list;
pub mod object;
pub mod pipeline;
#[cfg(windows)]
mod plaincopy;
#[cfg(windows)]
mod ranges;
#[cfg(windows)]
mod primitive;
#[cfg(all(windows, feature = "diagnostics"))]
mod profile;
pub mod query;
pub mod queue;
#[cfg(windows)]
mod queue2;
pub mod resource;
pub mod root;
#[cfg(windows)]
mod sampler;
#[cfg(windows)]
pub mod shader_cache;
#[cfg(windows)]
mod memory;
#[cfg(windows)]
mod shadow;
#[cfg(windows)]
mod tiles;
pub mod signature;
#[cfg(windows)]
mod texture;
#[cfg(windows)]
mod watch;
#[cfg(windows)]
mod texback;

#[cfg(windows)]
pub use host::on_failure;
#[cfg(windows)]
pub use inplace::{title_memory_freeing, title_memory_section, title_memory_view};
#[cfg(windows)]
pub use watch::{
    title_memory_reading, title_memory_remapped, title_memory_remapping, title_memory_written,
    writing as kernel_will_write,
};

/// Default ambient floor (`VICTORY_AMBIENT`) when unset.
pub const DEFAULT_AMBIENT: f32 = 0.6;

/// Engine-level graphics quality preset (`VICTORY_QUALITY`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quality {
    Low,
    Medium,
    High,
    Ultra,
}

impl Quality {
    /// Parses `low | medium | high | ultra` (case-insensitive).
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "low" => Some(Self::Low),
            "medium" => Some(Self::Medium),
            "high" => Some(Self::High),
            "ultra" => Some(Self::Ultra),
            _ => None,
        }
    }

    /// Canonical lowercase name of this preset.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Ultra => "ultra",
        }
    }

    /// Default texture budget in MiB for this preset (`None` uses the automatic VRAM share).
    pub const fn texture_budget_mb(self) -> Option<u64> {
        match self {
            Self::Low => Some(4096),
            Self::Medium => Some(5120),
            Self::High => Some(6144),
            Self::Ultra => None,
        }
    }

    /// Default ambient floor (`VICTORY_AMBIENT`) for this preset.
    pub const fn ambient(self) -> f32 {
        DEFAULT_AMBIENT
    }
}

/// Resolved engine-level graphics quality configuration.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GraphicsConfig {
    pub quality: Option<Quality>,
    pub texture_budget_mb: Option<u64>,
    pub ambient: Option<f32>,
}

impl GraphicsConfig {
    /// Resolves configuration from raw environment variable values without mutating process state.
    pub fn from_vars(
        quality: Option<&str>,
        texture_budget_mb: Option<&str>,
        ambient: Option<&str>,
    ) -> Self {
        let quality = resolve_quality(quality);
        Self {
            quality,
            texture_budget_mb: resolve_texture_budget_mb(texture_budget_mb, quality),
            ambient: resolve_ambient(ambient, quality),
        }
    }
}

/// Parses `VICTORY_QUALITY` (`None` when unset or empty).
pub fn resolve_quality(raw: Option<&str>) -> Option<Quality> {
    let raw = raw?.trim();
    if raw.is_empty() {
        return None;
    }
    Quality::parse(raw)
}

/// Resolves `VICTORY_TEXTURE_BUDGET_MB`, falling back to `quality` when unset or invalid.
pub fn resolve_texture_budget_mb(explicit: Option<&str>, quality: Option<Quality>) -> Option<u64> {
    if let Some(raw) = explicit.map(str::trim).filter(|s| !s.is_empty())
        && let Ok(mb) = raw.parse::<u64>()
    {
        return Some(mb);
    }
    quality.and_then(Quality::texture_budget_mb)
}

/// Resolves `VICTORY_AMBIENT` (`None` when `"off"`), falling back to `quality` then `0.6`.
pub fn resolve_ambient(explicit: Option<&str>, quality: Option<Quality>) -> Option<f32> {
    if let Some(raw) = explicit.map(str::trim).filter(|s| !s.is_empty()) {
        if raw.eq_ignore_ascii_case("off") {
            return None;
        }
        if let Ok(level) = raw.parse::<f32>()
            && level.is_finite()
            && level >= 0.0
        {
            return Some(level);
        }
    }
    Some(quality.map_or(DEFAULT_AMBIENT, Quality::ambient))
}

/// Process-wide graphics quality configuration, read once from `VICTORY_*` env vars.
pub fn graphics_config() -> GraphicsConfig {
    static CONFIG: std::sync::OnceLock<GraphicsConfig> = std::sync::OnceLock::new();
    *CONFIG.get_or_init(|| {
        let quality_raw = std::env::var("VICTORY_QUALITY").ok();
        let budget_raw = std::env::var("VICTORY_TEXTURE_BUDGET_MB").ok();
        let ambient_raw = std::env::var("VICTORY_AMBIENT").ok();

        if let Some(raw) = quality_raw.as_deref().map(str::trim).filter(|s| !s.is_empty())
            && Quality::parse(raw).is_none()
        {
            tracing::warn!(
                value = raw,
                "unknown VICTORY_QUALITY preset (expected low | medium | high | ultra); ignoring"
            );
        }

        GraphicsConfig::from_vars(
            quality_raw.as_deref(),
            budget_raw.as_deref(),
            ambient_raw.as_deref(),
        )
    })
}

/// Active texture upload budget in MiB, if capped by `VICTORY_TEXTURE_BUDGET_MB` or `VICTORY_QUALITY`.
pub fn texture_budget_mb() -> Option<u64> {
    graphics_config().texture_budget_mb
}

#[cfg(test)]
mod graphics_tests {
    use super::*;

    #[test]
    fn quality_presets_have_expected_defaults() {
        let low = GraphicsConfig::from_vars(Some("low"), None, None);
        assert_eq!(low.quality, Some(Quality::Low));
        assert_eq!(low.texture_budget_mb, Some(4096));
        assert_eq!(low.ambient, Some(0.6));

        let medium = GraphicsConfig::from_vars(Some("MEDIUM"), None, None);
        assert_eq!(medium.quality, Some(Quality::Medium));
        assert_eq!(medium.texture_budget_mb, Some(5120));
        assert_eq!(medium.ambient, Some(0.6));

        let high = GraphicsConfig::from_vars(Some(" High "), None, None);
        assert_eq!(high.quality, Some(Quality::High));
        assert_eq!(high.texture_budget_mb, Some(6144));
        assert_eq!(high.ambient, Some(0.6));

        let ultra = GraphicsConfig::from_vars(Some("ultra"), None, None);
        assert_eq!(ultra.quality, Some(Quality::Ultra));
        assert_eq!(ultra.texture_budget_mb, None);
        assert_eq!(ultra.ambient, Some(0.6));
    }

    #[test]
    fn explicit_env_vars_are_never_overridden_by_quality() {
        let cfg = GraphicsConfig::from_vars(Some("low"), Some("10240"), Some("0.9"));
        assert_eq!(cfg.quality, Some(Quality::Low));
        assert_eq!(cfg.texture_budget_mb, Some(10240));
        assert_eq!(cfg.ambient, Some(0.9));

        let ambient_off = GraphicsConfig::from_vars(Some("low"), None, Some("off"));
        assert_eq!(ambient_off.texture_budget_mb, Some(4096));
        assert_eq!(ambient_off.ambient, None);

        let ambient_zero = GraphicsConfig::from_vars(Some("medium"), None, Some("0"));
        assert_eq!(ambient_zero.ambient, Some(0.0));
    }

    #[test]
    fn unknown_or_empty_quality_uses_engine_defaults() {
        for raw in [None, Some(""), Some("   "), Some("potato")] {
            let cfg = GraphicsConfig::from_vars(raw, None, None);
            assert_eq!(cfg.quality, None);
            assert_eq!(cfg.texture_budget_mb, None);
            assert_eq!(cfg.ambient, Some(0.6));
        }
    }
}

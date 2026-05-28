//! User configuration loaded from `~/.config/dracoshell/config.toml`.
//!
//! Layout is intentionally minimal for the MVP; more fields land as the
//! renderer learns to honor them.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::Deserialize;

pub const DEFAULT_CONFIG_TOML: &str = r##"# dracoshell configuration
# Restart the terminal after changing this file.

[window]
width = 1200
height = 750

[font]
# Family resolved via fontconfig. Default has wide Unicode coverage so
# prompt glyphs (❯, , ➜) render. Other good choices: "JetBrains Mono",
# "Fira Code", "DejaVu Sans Mono", "Menlo".
family = "Hack"
size = 14.0

[colors]
# One Dark palette (matches VS Code / Atom). Background is slightly off-black
# for less eye strain than pure #000000.
background = "#1e2127"
foreground = "#dcdfe4"
unfocused = "#dcdfe4"
accent = "#FF2A2A"
"##;

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct Config {
    pub window: WindowConfig,
    pub font: FontConfig,
    pub colors: ColorsConfig,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct WindowConfig {
    pub width: u32,
    pub height: u32,
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self {
            width: 1200,
            height: 750,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct FontConfig {
    pub family: String,
    pub size: f32,
}

impl Default for FontConfig {
    fn default() -> Self {
        Self {
            family: "Hack".to_string(),
            size: 14.0,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct ColorsConfig {
    pub background: String,
    pub foreground: String,
    pub unfocused: String,
    pub accent: String,
    /// Built-in theme name (see `dracoshell --themes`). Overrides the
    /// individual color fields above when set to a known theme.
    pub theme: Option<String>,
}

impl Default for ColorsConfig {
    fn default() -> Self {
        Self {
            background: "#000000".to_string(),
            foreground: "#E6E6E6".to_string(),
            unfocused: "#8C8C8C".to_string(),
            accent: "#FF2A2A".to_string(),
            theme: Some("one-dark".to_string()),
        }
    }
}

pub fn config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("dracoshell").join("config.toml"))
}

pub fn exists() -> bool {
    config_path().map(|p| p.exists()).unwrap_or(false)
}

pub fn write_custom(font_size: f32, accent_hex: &str) -> Result<PathBuf> {
    let path = config_path().context("could not resolve user config dir")?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let content = format!(
        "# dracoshell configuration\n# Restart the terminal after changing this file.\n\n[window]\nwidth = 1200\nheight = 750\n\n[font]\nfamily = \"Hack\"\nsize = {font_size}\n\n[colors]\nbackground = \"#1e2127\"\nforeground = \"#dcdfe4\"\nunfocused = \"#dcdfe4\"\naccent = \"{accent_hex}\"\n"
    );
    fs::write(&path, content).with_context(|| format!("write {}", path.display()))?;
    Ok(path)
}

pub fn load() -> Config {
    let Some(path) = config_path() else {
        return Config::default();
    };
    let Ok(data) = fs::read_to_string(&path) else {
        return Config::default();
    };
    match toml::from_str::<Config>(&data) {
        Ok(cfg) => cfg,
        Err(e) => {
            log::warn!("invalid config at {}: {e}", path.display());
            Config::default()
        }
    }
}

pub fn write_default() -> Result<PathBuf> {
    let path = config_path().context("could not resolve user config dir")?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create {}", parent.display()))?;
    }
    fs::write(&path, DEFAULT_CONFIG_TOML)
        .with_context(|| format!("write {}", path.display()))?;
    Ok(path)
}

/// Updates `colors.theme = "<name>"` in the user's config, preserving all
/// other settings (and any custom keys we don't know about). Creates the
/// config file from defaults if it doesn't yet exist.
pub fn update_theme(name: &str) -> Result<PathBuf> {
    let path = config_path().context("could not resolve user config dir")?;
    if !path.exists() {
        write_default()?;
    }
    let content = fs::read_to_string(&path)
        .with_context(|| format!("read {}", path.display()))?;
    let mut value: toml::Value = content.parse().context("parse config toml")?;
    let table = value
        .as_table_mut()
        .ok_or_else(|| anyhow::anyhow!("config is not a table"))?;
    let colors = table
        .entry("colors".to_string())
        .or_insert_with(|| toml::Value::Table(Default::default()));
    let colors_tbl = colors
        .as_table_mut()
        .ok_or_else(|| anyhow::anyhow!("[colors] is not a table"))?;
    colors_tbl.insert("theme".to_string(), toml::Value::String(name.to_string()));
    let serialized = toml::to_string_pretty(&value).context("serialize config")?;
    fs::write(&path, serialized).with_context(|| format!("write {}", path.display()))?;
    Ok(path)
}

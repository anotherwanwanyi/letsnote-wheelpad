use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::{Error, Result};

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub device: Option<PathBuf>,
    pub device_name_regex: String,
    pub scroll: Scroll,
    pub log: Log,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Scroll {
    pub enable: bool,
    pub reverse_vertical: bool,
    pub horizontal_enable: bool,
    pub reverse_horizontal: bool,
    pub sensitivity: i32,
    pub detect_area_width: i32,
    pub horizontal_start: i32,
    pub horizontal_end: i32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Log {
    pub level: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            device: None,
            device_name_regex: "Synaptics.*TM3562".to_string(),
            scroll: Scroll::default(),
            log: Log::default(),
        }
    }
}

impl Default for Scroll {
    fn default() -> Self {
        // Defaults derived from the observed WheelPad.exe behaviour.
        // Sensitivity index 0 selects the multiplier 20 of
        // [5, 7, 10, 14, 20, 28, 40]. The two lowest entries are Linux
        // extensions for smoother slow scrolling.
        Self {
            enable: true,
            reverse_vertical: false,
            horizontal_enable: false,
            reverse_horizontal: false,
            sensitivity: 0,
            detect_area_width: 0,
            horizontal_start: 2,
            horizontal_end: 6,
        }
    }
}

impl Default for Log {
    fn default() -> Self {
        Self {
            level: "info".to_string(),
        }
    }
}

impl Config {
    /// Load from a TOML file. Missing file → defaults (warned by caller).
    pub fn load(path: &Path) -> Result<Self> {
        let text = match fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(source) => {
                return Err(Error::ConfigIo {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };
        let cfg: Config = toml::from_str(&text).map_err(|source| Error::ConfigParse {
            path: path.to_path_buf(),
            source,
        })?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn validate(&self) -> Result<()> {
        let s = &self.scroll;
        if !(-4..=2).contains(&s.sensitivity) {
            return Err(Error::ConfigRange {
                key: "scroll.sensitivity",
                value: s.sensitivity as i64,
                expected: "-4..=2",
            });
        }
        if !(0..=10).contains(&s.detect_area_width) {
            return Err(Error::ConfigRange {
                key: "scroll.detect_area_width",
                value: s.detect_area_width as i64,
                expected: "0..=10",
            });
        }
        if !(0..=15).contains(&s.horizontal_start) {
            return Err(Error::ConfigRange {
                key: "scroll.horizontal_start",
                value: s.horizontal_start as i64,
                expected: "0..=15",
            });
        }
        if !(0..=15).contains(&s.horizontal_end) {
            return Err(Error::ConfigRange {
                key: "scroll.horizontal_end",
                value: s.horizontal_end as i64,
                expected: "0..=15",
            });
        }
        Ok(())
    }

    /// Default path: `$XDG_CONFIG_HOME/letsnote-wheelpad/config.toml` falling
    /// back to `$HOME/.config/letsnote-wheelpad/config.toml`.
    pub fn default_path() -> PathBuf {
        if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
            if !xdg.is_empty() {
                return PathBuf::from(xdg)
                    .join("letsnote-wheelpad")
                    .join("config.toml");
            }
        }
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(home)
            .join(".config")
            .join("letsnote-wheelpad")
            .join("config.toml")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_windows() {
        let c = Config::default();
        assert!(c.scroll.enable);
        assert!(!c.scroll.reverse_vertical);
        assert!(!c.scroll.horizontal_enable);
        assert_eq!(c.scroll.sensitivity, 0);
        assert_eq!(c.scroll.detect_area_width, 0);
        assert_eq!(c.scroll.horizontal_start, 2);
        assert_eq!(c.scroll.horizontal_end, 6);
    }

    #[test]
    fn parses_partial_config() {
        let toml = r#"
            [scroll]
            sensitivity = -1
            horizontal_enable = true
        "#;
        let c: Config = toml::from_str(toml).unwrap();
        assert_eq!(c.scroll.sensitivity, -1);
        assert!(c.scroll.horizontal_enable);
        // unspecified keys keep defaults
        assert_eq!(c.scroll.horizontal_start, 2);
    }

    #[test]
    fn validate_rejects_out_of_range_sensitivity() {
        let mut c = Config::default();
        c.scroll.sensitivity = -5;
        assert!(c.validate().is_err());
        c.scroll.sensitivity = 3;
        assert!(c.validate().is_err());
    }

    #[test]
    fn validate_accepts_extended_low_sensitivity() {
        let mut c = Config::default();
        c.scroll.sensitivity = -4;
        assert!(c.validate().is_ok());
        c.scroll.sensitivity = -3;
        assert!(c.validate().is_ok());
    }
}

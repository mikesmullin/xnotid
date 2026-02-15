use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_monitor")]
    pub monitor: i32,

    #[serde(default = "default_position_x")]
    pub position_x: String, // "right", "left", "center"

    #[serde(default = "default_position_y")]
    pub position_y: String, // "top", "bottom"

    #[serde(default = "default_popup_width")]
    pub popup_width: i32,

    #[serde(default = "default_slot_height")]
    pub slot_height: i32,

    #[serde(default = "default_spacing")]
    pub spacing: i32,

    #[serde(default = "default_margin")]
    pub margin_top: i32,

    #[serde(default = "default_margin")]
    pub margin_right: i32,

    #[serde(default = "default_max_visible")]
    pub max_visible: i32,

    #[serde(default = "default_timeout_normal")]
    pub timeout_normal: u32, // seconds, 0 = persistent

    #[serde(default = "default_timeout_low")]
    pub timeout_low: u32,

    #[serde(default = "default_timeout_critical")]
    pub timeout_critical: u32,

    #[serde(default = "default_font_size_pct")]
    pub font_size_pct: f64, // like CSS rem, 100.0 = base

    #[serde(default = "default_animation_duration")]
    pub animation_duration_ms: u32,

    #[serde(default = "default_true")]
    pub hover_pause: bool,

    #[serde(default = "default_true")]
    pub click_to_dismiss: bool,

    #[serde(default)]
    pub close_button_on_hover: bool,

    #[serde(default = "default_scroll_speed")]
    pub scroll_speed: f64, // multiplier for scroll sensitivity, default 3.0

    #[serde(default = "default_max_popup_height")]
    pub max_popup_height: i32, // max popup window height in px, 0 = 80% of screen

    #[serde(default = "default_max_center_height")]
    pub max_center_height: i32, // max notification center height in px, 0 = 85% of screen

    #[serde(default = "default_true")]
    pub dnd_enabled: bool, // whether DND feature is available

    #[serde(default = "default_true")]
    pub log_enabled: bool,

    #[serde(default = "default_log_path")]
    pub log_path: String,
}

fn default_monitor() -> i32 { 0 }
fn default_position_x() -> String { "right".into() }
fn default_position_y() -> String { "top".into() }
fn default_popup_width() -> i32 { 400 }
fn default_slot_height() -> i32 { 75 }
fn default_spacing() -> i32 { 8 }
fn default_margin() -> i32 { 12 }
fn default_max_visible() -> i32 { 3 }
fn default_timeout_normal() -> u32 { 10 }
fn default_timeout_low() -> u32 { 5 }
fn default_timeout_critical() -> u32 { 0 }
fn default_font_size_pct() -> f64 { 100.0 }
fn default_animation_duration() -> u32 { 200 }
fn default_scroll_speed() -> f64 { 3.0 }
fn default_max_popup_height() -> i32 { 600 }
fn default_max_center_height() -> i32 { 600 }
fn default_true() -> bool { true }

fn default_log_path() -> String {
    let mut p = dirs::data_local_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
    p.push("xnotid");
    p.push("notifications.jsonl");
    p.to_string_lossy().into_owned()
}

impl Default for Config {
    fn default() -> Self {
        Self {
            monitor: default_monitor(),
            position_x: default_position_x(),
            position_y: default_position_y(),
            popup_width: default_popup_width(),
            slot_height: default_slot_height(),
            spacing: default_spacing(),
            margin_top: default_margin(),
            margin_right: default_margin(),
            max_visible: default_max_visible(),
            timeout_normal: default_timeout_normal(),
            timeout_low: default_timeout_low(),
            timeout_critical: default_timeout_critical(),
            font_size_pct: default_font_size_pct(),
            animation_duration_ms: default_animation_duration(),
            hover_pause: true,
            click_to_dismiss: true,
            close_button_on_hover: false,
            scroll_speed: default_scroll_speed(),
            max_popup_height: default_max_popup_height(),
            max_center_height: default_max_center_height(),
            dnd_enabled: true,
            log_enabled: true,
            log_path: default_log_path(),
        }
    }
}

impl Config {
    pub fn load() -> Self {
        let config_path = Self::config_path();
        if config_path.exists() {
            let contents = fs::read_to_string(&config_path).unwrap_or_default();
            serde_yaml::from_str(&contents).unwrap_or_else(|e| {
                log::warn!("Failed to parse config: {e}, using defaults");
                Config::default()
            })
        } else {
            log::info!("No config file found at {:?}, using defaults", config_path);
            Config::default()
        }
    }

    pub fn config_dir() -> PathBuf {
        let mut p = dirs::config_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
        p.push("xnotid");
        p
    }

    pub fn config_path() -> PathBuf {
        let mut p = Self::config_dir();
        p.push("config.yaml");
        p
    }

    pub fn css_path() -> PathBuf {
        let mut p = Self::config_dir();
        p.push("style.css");
        p
    }

    pub fn timeout_for_urgency(&self, urgency: u8) -> u32 {
        match urgency {
            0 => self.timeout_low,
            2 => self.timeout_critical,
            _ => self.timeout_normal,
        }
    }
}

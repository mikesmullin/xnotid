use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;
use zbus::zvariant::{OwnedValue, Value};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CardChoice {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum NotificationCard {
    MultipleChoice {
        question: String,
        choices: Vec<CardChoice>,
        #[serde(default)]
        allow_other: bool,
    },
    Permission {
        question: String,
        #[serde(default = "default_allow_label")]
        allow_label: String,
    },
}

fn default_allow_label() -> String {
    "Allow".to_string()
}

#[derive(Debug, Deserialize)]
struct CardEnvelope {
    #[serde(rename = "xnotid_card")]
    marker: String,
    #[serde(flatten)]
    card: NotificationCard,
}

/// Urgency levels per the freedesktop notification spec
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Urgency {
    Low = 0,
    Normal = 1,
    Critical = 2,
}

impl From<u8> for Urgency {
    fn from(v: u8) -> Self {
        match v {
            0 => Urgency::Low,
            2 => Urgency::Critical,
            _ => Urgency::Normal,
        }
    }
}

/// Close reasons per spec
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CloseReason {
    Expired = 1,
    Dismissed = 2,
    Closed = 3,
    Undefined = 4,
}

/// An action button attached to a notification
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Action {
    pub key: String,
    pub label: String,
}

/// Image data from hints (raw pixel data or a path/icon name)
#[derive(Debug, Clone)]
pub enum ImageData {
    Raw {
        width: i32,
        height: i32,
        rowstride: i32,
        has_alpha: bool,
        bits_per_sample: i32,
        channels: i32,
        data: Vec<u8>,
    },
    Path(String),
    Name(String),
    None,
}

/// Core notification data structure
#[derive(Debug, Clone)]
pub struct Notification {
    /// Internal auto-incrementing ID (matches D-Bus replaces_id protocol)
    pub id: u32,
    /// Unique identifier for logging
    pub uuid: String,
    /// Sending application name
    pub app_name: String,
    /// Summary / title
    pub summary: String,
    /// Body / detail text
    pub body: String,
    /// Icon name or path from the app_icon parameter
    pub app_icon: String,
    /// Action buttons
    pub actions: Vec<Action>,
    /// Urgency level
    pub urgency: Urgency,
    /// Timeout in seconds (0 = persistent / use config default, -1 = config default)
    pub timeout: i32,
    /// Group key — if set, notifications with same group collapse together
    pub group: Option<String>,
    /// Whether this notification requires an action button click to dismiss
    pub acknowledge_to_dismiss: bool,
    /// Image data extracted from hints
    pub image: ImageData,
    /// Timestamp when received
    pub created_at: DateTime<Utc>,
    /// Raw hints from D-Bus (non-image, for extensibility)
    pub hints: HashMap<String, String>,
    /// Desktop entry hint
    pub desktop_entry: Option<String>,
    /// Whether this is transient (popup only, no center storage)
    pub transient: bool,
    /// Progress value (0-100) if present
    pub progress: Option<i32>,
    /// Per-notification CSS class override
    pub css_class: Option<String>,
    /// Optional structured card payload parsed from body JSON
    pub card: Option<NotificationCard>,
}

impl Notification {
    fn parse_raw_image(hints: &HashMap<String, OwnedValue>, key: &str) -> Option<ImageData> {
        let raw = hints.get(key)?;
        let (width, height, rowstride, has_alpha, bits_per_sample, channels, data):
            (i32, i32, i32, bool, i32, i32, Vec<u8>) = raw.clone().try_into().ok()?;

        Some(ImageData::Raw {
            width,
            height,
            rowstride,
            has_alpha,
            bits_per_sample,
            channels,
            data,
        })
    }

    fn get_hint_string(hints: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
        hints.get(key).and_then(|v| {
            let val: Value<'_> = v.try_into().ok()?;
            match val {
                Value::Str(s) => Some(s.to_string()),
                _ => None,
            }
        })
    }

    fn get_hint_u8(hints: &HashMap<String, OwnedValue>, key: &str) -> Option<u8> {
        hints.get(key).and_then(|v| {
            let val: Value<'_> = v.try_into().ok()?;
            match val {
                Value::U8(n) => Some(n),
                _ => None,
            }
        })
    }

    fn get_hint_bool(hints: &HashMap<String, OwnedValue>, key: &str) -> Option<bool> {
        hints.get(key).and_then(|v| {
            let val: Value<'_> = v.try_into().ok()?;
            match val {
                Value::Bool(b) => Some(b),
                _ => None,
            }
        })
    }

    fn get_hint_i32(hints: &HashMap<String, OwnedValue>, key: &str) -> Option<i32> {
        hints.get(key).and_then(|v| {
            let val: Value<'_> = v.try_into().ok()?;
            match val {
                Value::I32(n) => Some(n),
                Value::U32(n) => Some(n as i32),
                _ => None,
            }
        })
    }

    pub fn new(
        id: u32,
        app_name: String,
        app_icon: String,
        summary: String,
        body: String,
        actions_raw: Vec<String>,
        hints: HashMap<String, OwnedValue>,
        expire_timeout: i32,
    ) -> Self {
        let card = Self::parse_card_body(&body);

        // Parse actions: they come as [key, label, key, label, ...]
        let actions: Vec<Action> = actions_raw
            .chunks(2)
            .filter_map(|chunk| {
                if chunk.len() == 2 {
                    Some(Action {
                        key: chunk[0].clone(),
                        label: chunk[1].clone(),
                    })
                } else {
                    None
                }
            })
            .collect();

        // Parse urgency from hints
        let urgency = Self::get_hint_u8(&hints, "urgency")
            .map(Urgency::from)
            .unwrap_or(Urgency::Normal);

        // Parse group from hints
        let group = Self::get_hint_string(&hints, "x-group");

        // Parse acknowledge-to-dismiss from hints
        let acknowledge_to_dismiss = Self::get_hint_bool(&hints, "x-acknowledge")
            .unwrap_or(false);

        let acknowledge_to_dismiss = acknowledge_to_dismiss || card.is_some();

        // Parse desktop entry
        let desktop_entry = Self::get_hint_string(&hints, "desktop-entry");

        // Parse transient
        let transient = Self::get_hint_bool(&hints, "transient")
            .unwrap_or(false);

        // Parse progress value
        let progress = Self::get_hint_i32(&hints, "value");

        // Parse CSS class override
        let css_class = Self::get_hint_string(&hints, "x-css-class");

        // Parse image data from hints
        let image = Self::parse_image(&hints, &app_icon);

        // Store simple string representations of remaining hints
        let hints_simple: HashMap<String, String> = hints
            .iter()
            .filter(|(k, _)| {
                !matches!(
                    k.as_str(),
                    "urgency"
                        | "image-data"
                        | "image_data"
                        | "image-path"
                        | "image_path"
                        | "icon_data"
                )
            })
            .map(|(k, v)| (k.clone(), format!("{:?}", v)))
            .collect();

        Self {
            id,
            uuid: Uuid::new_v4().to_string(),
            app_name,
            summary,
            body,
            app_icon,
            actions,
            urgency,
            timeout: expire_timeout,
            group,
            acknowledge_to_dismiss,
            image,
            created_at: Utc::now(),
            hints: hints_simple,
            desktop_entry,
            transient,
            progress,
            css_class,
            card,
        }
    }

    fn parse_card_body(body: &str) -> Option<NotificationCard> {
        let parsed = serde_json::from_str::<CardEnvelope>(body).ok()?;
        if parsed.marker != "v1" {
            return None;
        }
        Some(parsed.card)
    }

    fn parse_image(
        hints: &HashMap<String, OwnedValue>,
        app_icon: &str,
    ) -> ImageData {
        // Prefer raw image data if provided
        for key in &["image-data", "image_data", "icon_data"] {
            if let Some(raw) = Self::parse_raw_image(hints, key) {
                return raw;
            }
        }

        // Try image-path / image_path hints
        for key in &["image-path", "image_path"] {
            if let Some(path) = Self::get_hint_string(hints, key) {
                if !path.is_empty() {
                    // Distinguish path vs icon name
                    if path.starts_with('/') || path.starts_with("file://") {
                        return ImageData::Path(path);
                    } else {
                        return ImageData::Name(path);
                    }
                }
            }
        }

        // Fall back to app_icon parameter
        if !app_icon.is_empty() {
            if app_icon.starts_with('/') || app_icon.starts_with("file://") {
                return ImageData::Path(app_icon.to_string());
            } else {
                return ImageData::Name(app_icon.to_string());
            }
        }

        ImageData::None
    }
}

/// Log entry for the JSONL notification log
#[derive(Debug, Serialize, Deserialize)]
pub struct LogEntry {
    pub uuid: String,
    pub timestamp: String,
    pub event: String, // "received", "dismissed", "action", "expired", "closed"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notification_id: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_icon: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub urgency: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub desktop_entry: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hints: Option<HashMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
}

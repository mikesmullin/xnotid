use crate::config::Config;
use crate::notification::{CloseReason, LogEntry, Notification};
use chrono::Utc;
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Mutex};

/// Shared application state
pub struct Store {
    pub config: Config,
    /// All active notifications, keyed by ID
    pub notifications: HashMap<u32, Notification>,
    /// Display order (newest first)
    pub order: Vec<u32>,
    /// Group counters: group_key -> list of notification IDs in that group
    pub groups: HashMap<String, Vec<u32>>,
    /// Next ID to assign
    pub next_id: u32,
    /// Do Not Disturb state
    pub dnd: bool,
    /// IDs that were replaced in-place and need UI widget rebuild
    pub replaced_ids: Vec<u32>,
    /// Callback: notify the UI that something changed
    pub on_change: Option<Box<dyn Fn() + Send>>,
}

pub type SharedStore = Arc<Mutex<Store>>;

impl Store {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            notifications: HashMap::new(),
            order: Vec::new(),
            groups: HashMap::new(),
            next_id: 1,
            dnd: false,
            replaced_ids: Vec::new(),
            on_change: None,
        }
    }

    pub fn new_shared(config: Config) -> SharedStore {
        Arc::new(Mutex::new(Self::new(config)))
    }

    /// Add a notification, returning its assigned ID.
    /// If replaces_id > 0 and exists, replaces it.
    pub fn add(&mut self, mut noti: Notification, replaces_id: u32) -> u32 {
        let id = if replaces_id > 0 && self.notifications.contains_key(&replaces_id) {
            // Replace existing
            noti.id = replaces_id;
            self.notifications.insert(replaces_id, noti.clone());
            if !self.replaced_ids.contains(&replaces_id) {
                self.replaced_ids.push(replaces_id);
            }
            replaces_id
        } else {
            let id = self.next_id;
            self.next_id += 1;
            noti.id = id;

            // Handle grouping
            if let Some(ref group_key) = noti.group {
                self.groups
                    .entry(group_key.clone())
                    .or_default()
                    .push(id);
            }

            self.order.insert(0, id); // newest first
            self.notifications.insert(id, noti.clone());
            id
        };

        // Log
        self.log_event(&noti, "received", None);

        id
    }

    /// Close/remove a notification by ID with a reason.
    pub fn close(&mut self, id: u32, reason: CloseReason) -> Option<Notification> {
        if let Some(noti) = self.notifications.remove(&id) {
            self.order.retain(|&x| x != id);

            // Remove from group
            if let Some(ref group_key) = noti.group {
                if let Some(group) = self.groups.get_mut(group_key) {
                    group.retain(|&x| x != id);
                    if group.is_empty() {
                        self.groups.remove(group_key);
                    }
                }
            }

            let event = match reason {
                CloseReason::Expired => "expired",
                CloseReason::Dismissed => "dismissed",
                CloseReason::Closed => "closed",
                CloseReason::Undefined => "undefined",
            };
            self.log_event(&noti, event, None);

            Some(noti)
        } else {
            None
        }
    }

    /// Record an action invocation
    pub fn log_action(&self, id: u32, action_key: &str) {
        if let Some(noti) = self.notifications.get(&id) {
            self.log_event(noti, "action", Some(action_key.to_string()));
        }
    }

    /// Get visible notifications (respecting DND)
    pub fn visible_popups(&self) -> Vec<&Notification> {
        self.order
            .iter()
            .filter_map(|id| self.notifications.get(id))
            .filter(|n| {
                if self.dnd && n.urgency != crate::notification::Urgency::Critical {
                    return false;
                }
                true
            })
            .collect()
    }

    /// Get all notifications for the notification center
    pub fn all_notifications(&self) -> Vec<&Notification> {
        self.order
            .iter()
            .filter_map(|id| self.notifications.get(id))
            .filter(|n| !n.transient)
            .collect()
    }

    /// Clear all notifications
    pub fn clear_all(&mut self) {
        let ids: Vec<u32> = self.order.clone();
        for id in ids {
            self.close(id, CloseReason::Dismissed);
        }
    }

    fn log_event(&self, noti: &Notification, event: &str, action_key: Option<String>) {
        if !self.config.log_enabled {
            return;
        }

        let entry = LogEntry {
            uuid: noti.uuid.clone(),
            timestamp: Utc::now().to_rfc3339(),
            event: event.to_string(),
            notification_id: Some(noti.id),
            app_name: Some(noti.app_name.clone()),
            app_icon: if event == "received" {
                Some(noti.app_icon.clone())
            } else {
                None
            },
            summary: Some(noti.summary.clone()),
            body: if event == "received" {
                Some(noti.body.clone())
            } else {
                None
            },
            created_at: if event == "received" {
                Some(noti.created_at.to_rfc3339())
            } else {
                None
            },
            urgency: if event == "received" {
                Some(format!("{:?}", noti.urgency))
            } else {
                None
            },
            desktop_entry: if event == "received" {
                noti.desktop_entry.clone()
            } else {
                None
            },
            hints: if event == "received" {
                Some(noti.hints.clone())
            } else {
                None
            },
            action_key,
            group: noti.group.clone(),
        };

        if let Ok(json) = serde_json::to_string(&entry) {
            let log_path = &self.config.log_path;
            if let Some(parent) = Path::new(log_path).parent() {
                let _ = fs::create_dir_all(parent);
            }
            if let Ok(mut f) = OpenOptions::new()
                .create(true)
                .append(true)
                .open(log_path)
            {
                let _ = writeln!(f, "{}", json);
            }
        }
    }

    pub fn notify_change(&self) {
        if let Some(ref cb) = self.on_change {
            cb();
        }
    }

    pub fn take_replaced_ids(&mut self) -> Vec<u32> {
        std::mem::take(&mut self.replaced_ids)
    }
}

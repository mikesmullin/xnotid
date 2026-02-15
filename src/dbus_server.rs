use crate::notification::{CloseReason, Notification};
use crate::store::SharedStore;
use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender};
use zbus::object_server::SignalEmitter;
use zbus::zvariant::OwnedValue;
use zbus::{interface, Connection};
use gtk4::glib as glib2;

/// Commands that can be sent from D-Bus to the UI thread
#[derive(Debug)]
pub enum UiCommand {
    ToggleCenter,
}

/// Signals that should be emitted on D-Bus (sent from UI thread)
#[derive(Debug)]
pub enum DbusSignal {
    ActionInvoked { id: u32, action_key: String },
}

/// The D-Bus notification server implementing org.freedesktop.Notifications
pub struct NotificationServer {
    store: SharedStore,
}

impl NotificationServer {
    pub fn new(store: SharedStore) -> Self {
        Self { store }
    }
}

#[interface(name = "org.freedesktop.Notifications")]
impl NotificationServer {
    /// Returns the capabilities of this notification server.
    fn get_capabilities(&self) -> Vec<String> {
        vec![
            "body".into(),
            "body-markup".into(),
            "body-images".into(),
            "actions".into(),
            "persistence".into(),
            "icon-static".into(),
        ]
    }

    /// Sends a notification. Returns the notification ID.
    fn notify(
        &mut self,
        app_name: &str,
        replaces_id: u32,
        app_icon: &str,
        summary: &str,
        body: &str,
        actions: Vec<String>,
        hints: HashMap<String, OwnedValue>,
        expire_timeout: i32,
    ) -> u32 {
        log::info!(
            "Notify: app={}, summary={}, replaces={}",
            app_name,
            summary,
            replaces_id
        );

        let noti = Notification::new(
            0, // will be assigned by store
            app_name.to_string(),
            app_icon.to_string(),
            summary.to_string(),
            body.to_string(),
            actions,
            hints,
            expire_timeout,
        );

        let mut store = self.store.lock().unwrap();
        let id = store.add(noti, replaces_id);
        drop(store);

        // Trigger UI update via glib main context
        let store_clone = self.store.clone();
        glib2::idle_add_once(move || {
            let s = store_clone.lock().unwrap();
            s.notify_change();
        });

        id
    }

    /// Closes a notification by ID.
    fn close_notification(
        &self,
        id: u32,
    ) {
        log::info!("CloseNotification: id={}", id);
        let mut store = self.store.lock().unwrap();
        store.close(id, CloseReason::Closed);
        store.notify_change();
    }

    /// Returns server information.
    fn get_server_information(&self) -> (String, String, String, String) {
        (
            "xnotid".into(),
            "xnotid".into(),
            env!("CARGO_PKG_VERSION").into(),
            "1.2".into(),
        )
    }

    /// Signal: NotificationClosed(id, reason)
    #[zbus(signal)]
    async fn notification_closed(
        emitter: &SignalEmitter<'_>,
        id: u32,
        reason: u32,
    ) -> zbus::Result<()>;

    /// Signal: ActionInvoked(id, action_key)
    #[zbus(signal)]
    async fn action_invoked(
        emitter: &SignalEmitter<'_>,
        id: u32,
        action_key: &str,
    ) -> zbus::Result<()>;
}

/// Control interface for xnotid-specific commands
pub struct ControlServer {
    cmd_tx: Sender<UiCommand>,
}

impl ControlServer {
    pub fn new(cmd_tx: Sender<UiCommand>) -> Self {
        Self { cmd_tx }
    }
}

#[interface(name = "org.xnotid.Control")]
impl ControlServer {
    /// Toggle the notification center visibility
    fn toggle_center(&self) {
        log::info!("ToggleCenter requested via D-Bus");
        let _ = self.cmd_tx.send(UiCommand::ToggleCenter);
    }
}

/// Starts the D-Bus server and acquires the notification bus name.
pub async fn start_dbus_server(
    store: SharedStore,
    cmd_tx: Sender<UiCommand>,
    signal_rx: Receiver<DbusSignal>,
) -> zbus::Result<Connection> {
    let server = NotificationServer::new(store);
    let control = ControlServer::new(cmd_tx);

    let connection = Connection::session().await?;

    connection
        .object_server()
        .at("/org/freedesktop/Notifications", server)
        .await?;

    connection
        .object_server()
        .at("/org/xnotid/Control", control)
        .await?;

    connection
        .request_name("org.freedesktop.Notifications")
        .await?;

    connection
        .request_name("org.xnotid.Control")
        .await?;

    log::info!("D-Bus server started: org.freedesktop.Notifications + org.xnotid.Control");

    // Spawn task to handle signal emissions from UI thread
    let conn_clone = connection.clone();
    tokio::spawn(async move {
        loop {
            // Check for signals to emit (non-blocking poll)
            match signal_rx.try_recv() {
                Ok(DbusSignal::ActionInvoked { id, action_key }) => {
                    log::info!("Emitting ActionInvoked signal: id={}, key={}", id, action_key);
                    let iface_ref = conn_clone
                        .object_server()
                        .interface::<_, NotificationServer>("/org/freedesktop/Notifications")
                        .await;
                    if let Ok(iface) = iface_ref {
                        let _ = NotificationServer::action_invoked(
                            iface.signal_emitter(),
                            id,
                            &action_key,
                        )
                        .await;
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    // No signals pending, sleep briefly
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    log::warn!("Signal channel disconnected");
                    break;
                }
            }
        }
    });

    Ok(connection)
}

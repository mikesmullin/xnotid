mod config;
mod dbus_server;
mod notification;
mod store;
mod tray;
mod ui;

use config::Config;
use dbus_server::{DbusSignal, UiCommand};
use std::rc::Rc;
use store::Store;

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    log::info!("xnotid starting");

    gtk4::init().expect("Failed to initialize GTK4");

    let config = Config::load();
    log::info!(
        "Config loaded: popup_width={}, max_visible={}",
        config.popup_width,
        config.max_visible
    );

    let store = Store::new_shared(config);

    // Signal channel for UI -> D-Bus (e.g. ActionInvoked)
    let (signal_tx, signal_rx) = std::sync::mpsc::channel::<DbusSignal>();

    // Create UI (no Application — we manage our own main loop)
    let ui = Rc::new(ui::Ui::new(store.clone(), signal_tx));

    // Wire up store -> UI refresh callback via channel
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    {
        let mut s = store.lock().unwrap();
        s.on_change = Some(Box::new(move || {
            let _ = tx.send(());
        }));
    }

    // Poll the channel from the GTK main loop
    let ui_poll = ui.clone();
    glib2::timeout_add_local(std::time::Duration::from_millis(50), move || {
        while let Ok(()) = rx.try_recv() {
            ui_poll.refresh();
        }
        glib2::ControlFlow::Continue
    });

    // Command channel for D-Bus -> UI (e.g. toggle center)
    let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<UiCommand>();
    let ui_cmd = ui.clone();
    glib2::timeout_add_local(std::time::Duration::from_millis(50), move || {
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                UiCommand::ToggleCenter => ui_cmd.toggle_center(),
            }
        }
        glib2::ControlFlow::Continue
    });

    tray::start_tray_service(cmd_tx.clone());

    // Position popup window (hidden until first notification)
    ui.position_popup();

    // Start D-Bus server in a background thread
    let store_dbus = store.clone();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
        rt.block_on(async {
            match dbus_server::start_dbus_server(store_dbus, cmd_tx, signal_rx).await {
                Ok(_conn) => {
                    log::info!("D-Bus connection established");
                    loop {
                        tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
                    }
                }
                Err(e) => {
                    log::error!("Failed to start D-Bus server: {}", e);
                    std::process::exit(1);
                }
            }
        });
    });

    log::info!("xnotid ready, entering main loop");

    // Run the GLib main loop — runs forever (daemon)
    let main_loop = glib2::MainLoop::new(None, false);
    main_loop.run();
}

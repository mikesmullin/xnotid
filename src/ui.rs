use crate::config::Config;
use crate::dbus_server::DbusSignal;
use crate::notification::{CloseReason, ImageData, Notification, Urgency};
use crate::store::SharedStore;
use gdk4::gdk_pixbuf::Pixbuf;
use gtk4::prelude::*;
use gtk4::{
    Align, Box as GtkBox, Button, CssProvider, EventControllerKey, GestureClick, Image, Label,
    Orientation, Revealer, RevealerTransitionType, ScrolledWindow, Window,
};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

/// Manages the popup notification window and the notification center panel.
pub struct Ui {
    store: SharedStore,
    config: Config,
    /// Sender for D-Bus signals (ActionInvoked, etc.)
    signal_tx: std::sync::mpsc::Sender<DbusSignal>,
    /// The popup window (always present, visibility toggled)
    popup_window: Window,
    /// Max popup height in pixels (80% of screen)
    max_popup_h: i32,
    /// The popup container holding notification slots
    popup_box: GtkBox,
    /// Map of notification ID -> popup widget for removal
    popup_widgets: Rc<RefCell<HashMap<u32, GtkBox>>>,
    /// Timeout source IDs for auto-dismiss
    timeout_sources: Rc<RefCell<HashMap<u32, glib2::SourceId>>>,
    /// The notification center window
    center_window: Window,
    /// The notification center list container
    center_box: GtkBox,
    /// Map of notification ID -> center widget for removal
    center_widgets: Rc<RefCell<HashMap<u32, GtkBox>>>,
}

impl Ui {
    const POPUP_ANIMATION_MS: u32 = 500;

    pub fn new(store: SharedStore, signal_tx: std::sync::mpsc::Sender<DbusSignal>) -> Self {
        let config = {
            let s = store.lock().unwrap();
            s.config.clone()
        };

        // Load CSS
        Self::load_css(&config);

        // Detect screen height upfront (display available after gtk4::init)
        let display = gdk4::Display::default().expect("No display");
        let monitors = gdk4::prelude::DisplayExt::monitors(&display);
        let monitor_idx = config.monitor.min(monitors.n_items() as i32 - 1).max(0);
        let screen_h = if let Some(obj) = monitors.item(monitor_idx as u32) {
            if let Ok(mon) = obj.downcast::<gdk4::Monitor>() {
                gdk4::prelude::MonitorExt::geometry(&mon).height()
            } else { 1080 }
        } else { 1080 };

        // Compute max heights: use config if > 0, else fall back to screen percentage
        let screen_popup_max = (screen_h as f64 * 0.8) as i32;
        let screen_center_max = (screen_h as f64 * 0.85) as i32;
        let max_popup_h = if config.max_popup_height > 0 {
            config.max_popup_height.min(screen_popup_max)
        } else {
            screen_popup_max
        };
        let max_center_h = if config.max_center_height > 0 {
            config.max_center_height.min(screen_center_max)
        } else {
            screen_center_max
        };
        log::info!("Screen {}px → popup max {}px, center max {}px", screen_h, max_popup_h, max_center_h);

        // Create popup window
        let popup_window = Window::builder()
            .title("xnotid-popups")
            .decorated(false)
            .default_width(config.popup_width)
            .css_name("popup-window")
            .build();

        popup_window.set_widget_name("xnotid-popup-window");

        let popup_scroll = ScrolledWindow::new();
        popup_scroll.set_widget_name("popup-scroll");
        popup_scroll.set_vexpand(true);
        popup_scroll.set_hscrollbar_policy(gtk4::PolicyType::Never);
        popup_scroll.set_vscrollbar_policy(gtk4::PolicyType::Automatic);
        popup_scroll.set_propagate_natural_height(true);
        popup_scroll.set_max_content_height(max_popup_h);

        // Boost scroll speed
        let adj = popup_scroll.vadjustment();
        let step = (adj.step_increment() * config.scroll_speed).max(30.0);
        adj.set_step_increment(step);
        adj.set_page_increment(step * 3.0);

        let popup_box = GtkBox::new(Orientation::Vertical, config.spacing);
        popup_box.set_widget_name("popup-container");
        popup_scroll.set_child(Some(&popup_box));
        popup_window.set_child(Some(&popup_scroll));

        // Position the popup window
        // GTK4 on X11: we set the window as a popup and position it
        popup_window.set_visible(false);

        // Create notification center window
        let center_window = Window::builder()
            .title("xnotid-center")
            .decorated(false)
            .resizable(false)
            .default_width(config.popup_width)
            .default_height(max_center_h)
            .css_name("center-window")
            .build();
        center_window.set_widget_name("xnotid-center-window");
        center_window.set_focusable(true);

        let center_main_box = GtkBox::new(Orientation::Vertical, 0);
        center_main_box.set_widget_name("center-main");

        // Header with title + DND + Clear All
        let header_box = GtkBox::new(Orientation::Horizontal, 8);
        header_box.set_widget_name("center-header");
        header_box.set_margin_start(12);
        header_box.set_margin_end(12);
        header_box.set_margin_top(8);
        header_box.set_margin_bottom(8);

        let title_label = Label::new(Some("Notifications"));
        title_label.set_widget_name("center-title");
        title_label.set_hexpand(true);
        title_label.set_halign(Align::Start);
        header_box.append(&title_label);

        // DND toggle button
        let dnd_btn = Button::with_label("DND");
        dnd_btn.set_widget_name("dnd-button");
        let store_dnd = store.clone();
        dnd_btn.connect_clicked(move |btn| {
            let mut s = store_dnd.lock().unwrap();
            s.dnd = !s.dnd;
            if s.dnd {
                btn.add_css_class("active");
            } else {
                btn.remove_css_class("active");
            }
            log::info!("DND toggled: {}", s.dnd);
            s.notify_change();
        });
        header_box.append(&dnd_btn);

        // Clear all button
        let clear_btn = Button::with_label("Clear All");
        clear_btn.set_widget_name("clear-all-button");
        let store_clear = store.clone();
        clear_btn.connect_clicked(move |_| {
            let mut s = store_clear.lock().unwrap();
            s.clear_all();
            s.notify_change();
            log::info!("All notifications cleared");
        });
        header_box.append(&clear_btn);

        center_main_box.append(&header_box);

        // Scrollable notification list
        let scrolled = ScrolledWindow::new();
        scrolled.set_vexpand(true);
        scrolled.set_widget_name("center-scroll");
        scrolled.set_hscrollbar_policy(gtk4::PolicyType::Never);
        scrolled.set_vscrollbar_policy(gtk4::PolicyType::Automatic);
        scrolled.set_max_content_height(max_center_h);
        scrolled.set_propagate_natural_height(true);

        // Boost scroll speed
        let center_adj = scrolled.vadjustment();
        let center_step = (center_adj.step_increment() * config.scroll_speed).max(30.0);
        center_adj.set_step_increment(center_step);
        center_adj.set_page_increment(center_step * 3.0);

        let center_box = GtkBox::new(Orientation::Vertical, config.spacing);
        center_box.set_widget_name("center-list");
        center_box.set_margin_start(8);
        center_box.set_margin_end(8);
        center_box.set_margin_top(4);
        center_box.set_margin_bottom(4);
        scrolled.set_child(Some(&center_box));

        // Empty state placeholder
        let empty_label = Label::new(Some("No Notifications"));
        empty_label.set_widget_name("center-empty");
        empty_label.set_css_classes(&["dim-label"]);
        center_box.append(&empty_label);

        center_main_box.append(&scrolled);
        center_window.set_child(Some(&center_main_box));
        center_window.set_visible(false);

        let popup_widgets = Rc::new(RefCell::new(HashMap::new()));

        // ESC on center window should behave like tray bell toggle
        let center_for_esc = center_window.clone();
        let popup_for_esc = popup_window.clone();
        let key_controller = EventControllerKey::new();
        let popup_widgets_for_esc = popup_widgets.clone();

        key_controller.connect_key_pressed(move |_, key, _, _| {
            if key == gdk4::Key::Escape {
                if center_for_esc.is_visible() {
                    center_for_esc.set_visible(false);
                    if !popup_widgets_for_esc.borrow().is_empty() {
                        popup_for_esc.set_visible(true);
                        popup_for_esc.present();
                    }
                } else {
                    popup_for_esc.set_visible(false);
                    center_for_esc.present();
                }
                glib2::Propagation::Stop
            } else {
                glib2::Propagation::Proceed
            }
        });
        center_window.add_controller(key_controller);

        Self {
            store,
            config,
            signal_tx,
            popup_window,
            max_popup_h,
            popup_box,
            popup_widgets,
            timeout_sources: Rc::new(RefCell::new(HashMap::new())),
            center_window,
            center_box,
            center_widgets: Rc::new(RefCell::new(HashMap::new())),
        }
    }

    fn load_css(_config: &Config) {
        let css_path = Config::css_path();

        // If no CSS file on disk yet, write the built-in default
        if !css_path.exists() {
            if let Some(parent) = css_path.parent() {
                std::fs::create_dir_all(parent).ok();
            }
            std::fs::write(&css_path, include_str!("default.css")).ok();
            log::info!("Wrote default CSS to {:?}", css_path);
        }

        // Always load CSS from disk so edits take effect on restart
        let provider = CssProvider::new();
        provider.load_from_path(css_path.to_str().unwrap_or_default());
        log::info!("Loaded CSS from {:?}", css_path);

        gtk4::style_context_add_provider_for_display(
            &gdk4::Display::default().expect("No display"),
            &provider,
            gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }

    /// Position the popup window at the configured screen corner.
    pub fn position_popup(&self) {
        // GTK4 doesn't have direct window.move() — we rely on the WM
        // For X11 override-redirect approach, we'd need to use X11 surface directly.
        // For now, present the window and let it be positioned.
        // Real positioning will be done after realize via the X11 surface.
        self.popup_window.present();

        // After the window is realized, position it
        let config = self.config.clone();
        let popup = self.popup_window.clone();
        popup.connect_realize(move |window| {
            Self::position_window_x11(window, &config, true);
        });
    }

    fn position_window_x11(window: &Window, config: &Config, is_popup: bool) {
        // On X11 with GTK4, positioning is best-effort.
        let display = gtk4::prelude::WidgetExt::display(window);
        let monitors = gdk4::prelude::DisplayExt::monitors(&display);
        let monitor_idx = config.monitor.min(monitors.n_items() as i32 - 1).max(0);
        if let Some(obj) = monitors.item(monitor_idx as u32) {
            if let Ok(monitor) = obj.downcast::<gdk4::Monitor>() {
                let geom = gdk4::prelude::MonitorExt::geometry(&monitor);

                let w = config.popup_width;
                let h = if is_popup { 1 } else { 600 };

                let x = match config.position_x.as_str() {
                    "left" => geom.x() + config.margin_right,
                    "center" => geom.x() + (geom.width() - w) / 2,
                    _ => geom.x() + geom.width() - w - config.margin_right,
                };

                let y = match config.position_y.as_str() {
                    "bottom" => geom.y() + geom.height() - h - config.margin_top,
                    _ => geom.y() + config.margin_top,
                };

                log::info!("Positioning window at ({}, {})", x, y);
            }
        }
    }

    /// Show a notification popup
    pub fn show_notification(&self, noti: &Notification) {
        let id = noti.id;
        let config = self.config.clone();
        let was_empty = self.popup_widgets.borrow().is_empty();

        // Compute effective timeout upfront (needed by widget for hover-pause)
        // D-Bus spec: -1 = server decides, 0 = never expire, >0 = ms
        let effective_timeout = if noti.timeout == 0 {
            0 // never expire
        } else if noti.timeout < 0 {
            config.timeout_for_urgency(noti.urgency as u8) // server decides (seconds)
        } else {
            // Client-specified timeout in milliseconds, convert to seconds (min 1s)
            ((noti.timeout as u32) / 1000).max(1)
        };

        // Build the notification widget
        let slot = self.build_notification_widget(noti, true, effective_timeout);

        // Wrap in a Revealer for animation
        let revealer = Revealer::new();
        revealer.set_transition_type(RevealerTransitionType::SlideDown);
        revealer.set_transition_duration(Self::POPUP_ANIMATION_MS);
        revealer.set_child(Some(&slot));
        revealer.set_reveal_child(false);

        let slot_wrapper = GtkBox::new(Orientation::Vertical, 0);
        slot_wrapper.append(&revealer);
        slot_wrapper.set_opacity(0.0);

        self.popup_box.append(&slot_wrapper);
        self.popup_widgets
            .borrow_mut()
            .insert(id, slot_wrapper.clone());

        let popup_is_visible = !self.center_window.is_visible();
        if popup_is_visible {
            self.popup_window.set_visible(true);
            self.popup_window.present();
        }

        if was_empty {
            let target_h = Self::first_popup_target_height(
                &self.popup_box,
                &slot,
                self.config.popup_width,
                self.max_popup_h,
            );

            self.popup_window
                .set_default_size(self.config.popup_width, target_h);
            self.popup_window.queue_resize();

            let revealer_for_appear = revealer.clone();
            let slot_wrapper_for_appear = slot_wrapper.clone();
            if popup_is_visible {
                Self::start_appear_after_resize(
                    self.popup_window.clone(),
                    revealer_for_appear,
                    slot_wrapper_for_appear,
                    target_h,
                );
            } else {
                glib2::timeout_add_local_once(std::time::Duration::from_millis(16), move || {
                    Self::begin_appear_animation(revealer_for_appear, slot_wrapper_for_appear);
                });
            }
        } else {
            let target_h = Self::projected_popup_height(
                &self.popup_box,
                self.config.popup_width,
                self.max_popup_h,
            );

            self.popup_window
                .set_default_size(self.config.popup_width, target_h);
            self.popup_window.queue_resize();

            let revealer_for_appear = revealer.clone();
            let slot_wrapper_for_appear = slot_wrapper.clone();
            if popup_is_visible {
                Self::start_appear_after_resize(
                    self.popup_window.clone(),
                    revealer_for_appear,
                    slot_wrapper_for_appear,
                    target_h,
                );
            } else {
                glib2::timeout_add_local_once(std::time::Duration::from_millis(16), move || {
                    Self::begin_appear_animation(revealer_for_appear, slot_wrapper_for_appear);
                });
            }
        }

        // Schedule auto-dismiss timeout
        if effective_timeout > 0 && !noti.acknowledge_to_dismiss {
            let store = self.store.clone();
            let widgets = self.popup_widgets.clone();
            let timeouts = self.timeout_sources.clone();
            let popup_box = self.popup_box.clone();
            let popup_window = self.popup_window.clone();

            let source_id = glib2::timeout_add_seconds_local_once(effective_timeout, move || {
                Self::dismiss_popup_static(
                    id,
                    &store,
                    &widgets,
                    &timeouts,
                    &popup_box,
                    &popup_window,
                );
            });

            self.timeout_sources.borrow_mut().insert(id, source_id);
        }
        // Note: center widget is added by refresh(), not here
    }

    /// Build a notification widget (used for both popup and center)
    fn build_notification_widget(&self, noti: &Notification, is_popup: bool, effective_timeout: u32) -> GtkBox {
        let slot = GtkBox::new(Orientation::Horizontal, 8);
        slot.set_widget_name("notification");
        slot.set_css_classes(&[
            "notification",
            match noti.urgency {
                Urgency::Low => "low",
                Urgency::Normal => "normal",
                Urgency::Critical => "critical",
            },
        ]);

        if let Some(ref class) = noti.css_class {
            slot.add_css_class(class);
        }

        slot.set_margin_start(8);
        slot.set_margin_end(8);
        slot.set_margin_top(4);
        slot.set_margin_bottom(4);

        // Icon / Image (float left)
        if let Some(img) = self.build_image(&noti.image) {
            img.set_widget_name("notification-icon");
            img.set_hexpand(false);
            img.set_vexpand(false);
            img.set_valign(Align::Start);
            img.set_halign(Align::Start);
            slot.append(&img);
        }

        // Text content
        let text_box = GtkBox::new(Orientation::Vertical, 2);
        text_box.set_hexpand(true);
        text_box.set_widget_name("notification-text");

        // Summary
        let summary = Label::new(Some(&noti.summary));
        summary.set_widget_name("notification-summary");
        summary.set_css_classes(&["summary"]);
        summary.set_halign(Align::Start);
        summary.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        summary.set_max_width_chars(40);
        text_box.append(&summary);

        // Body
        let body_is_truncated = Rc::new(RefCell::new(false));
        if !noti.body.is_empty() {
            let body = Label::new(Some(&noti.body));
            body.set_widget_name("notification-body");
            body.set_css_classes(&["body"]);
            body.set_halign(Align::Start);
            body.set_wrap(true);
            body.set_max_width_chars(50);
            body.set_use_markup(true);

            if is_popup {
                body.set_lines(2);
                body.set_ellipsize(gtk4::pango::EllipsizeMode::End);
                // Check if text would be truncated (heuristic: body > ~100 chars)
                if noti.body.len() > 100 {
                    *body_is_truncated.borrow_mut() = true;
                }
            }
            text_box.append(&body);
        }

        // Action buttons
        if !noti.actions.is_empty() {
            let actions_box = GtkBox::new(Orientation::Horizontal, 4);
            actions_box.set_widget_name("notification-actions");
            actions_box.set_margin_top(4);

            for action in &noti.actions {
                let btn = Button::with_label(&action.label);
                btn.set_css_classes(&["notification-action"]);
                let store = self.store.clone();
                let signal_tx = self.signal_tx.clone();
                let action_key = action.key.clone();
                let noti_id = noti.id;
                btn.connect_clicked(move |_| {
                    log::info!("Action invoked: id={} key={}", noti_id, action_key);
                    // Emit D-Bus ActionInvoked signal
                    let _ = signal_tx.send(DbusSignal::ActionInvoked {
                        id: noti_id,
                        action_key: action_key.clone(),
                    });
                    // Dismiss the notification after action
                    let mut s = store.lock().unwrap();
                    s.log_action(noti_id, &action_key);
                    s.close(noti_id, CloseReason::Dismissed);
                    s.notify_change();
                });
                actions_box.append(&btn);
            }
            text_box.append(&actions_box);
        }

        // Progress bar
        if let Some(progress) = noti.progress {
            let pbar = gtk4::ProgressBar::new();
            pbar.set_fraction(progress as f64 / 100.0);
            pbar.set_widget_name("notification-progress");
            text_box.append(&pbar);
        }

        slot.append(&text_box);

        // Close button (only if configured)
        if self.config.close_button_on_hover {
            let close_btn = Button::with_label("×");
            close_btn.set_css_classes(&["close-button"]);
            close_btn.set_valign(Align::Start);
            close_btn.set_opacity(0.0);

            let close_ref = close_btn.clone();
            let hover_enter = gtk4::EventControllerMotion::new();
            let close_show = close_ref.clone();
            hover_enter.connect_enter(move |_, _, _| {
                close_show.set_opacity(1.0);
            });

            let hover_leave = gtk4::EventControllerMotion::new();
            let close_hide = close_ref.clone();
            hover_leave.connect_leave(move |_| {
                close_hide.set_opacity(0.0);
            });

            slot.add_controller(hover_enter);
            slot.add_controller(hover_leave);

            // Close button action
            if is_popup {
                let store = self.store.clone();
                let noti_id = noti.id;
                let widgets = self.popup_widgets.clone();
                let timeouts = self.timeout_sources.clone();
                let popup_box = self.popup_box.clone();
                let popup_window = self.popup_window.clone();
                close_btn.connect_clicked(move |_| {
                    Self::dismiss_popup_static(
                        noti_id, &store, &widgets, &timeouts, &popup_box, &popup_window,
                    );
                });
            } else {
                let store = self.store.clone();
                let noti_id = noti.id;
                let center_widgets = self.center_widgets.clone();
                let center_box = self.center_box.clone();
                close_btn.connect_clicked(move |_| {
                    Self::dismiss_center_static(noti_id, &store, &center_widgets, &center_box);
                });
            }

            slot.append(&close_btn);
        }

        // Hover-to-pause timeout (popup only)
        if self.config.hover_pause && is_popup {
            let timeouts = self.timeout_sources.clone();
            let noti_id = noti.id;
            let widgets = self.popup_widgets.clone();
            let popup_box = self.popup_box.clone();
            let popup_window = self.popup_window.clone();
            let timeouts_clone = self.timeout_sources.clone();
            let original_timeout = effective_timeout;

            let hover_ctrl = gtk4::EventControllerMotion::new();
            let timeouts_enter = timeouts.clone();
            hover_ctrl.connect_enter(move |_, _, _| {
                if let Some(source_id) = timeouts_enter.borrow_mut().remove(&noti_id) {
                    source_id.remove();
                }
            });

            let store2 = self.store.clone();
            hover_ctrl.connect_leave(move |_| {
                if original_timeout > 0 {
                    let store3 = store2.clone();
                    let widgets2 = widgets.clone();
                    let timeouts3 = timeouts_clone.clone();
                    let popup_box2 = popup_box.clone();
                    let popup_win2 = popup_window.clone();
                    let sid = glib2::timeout_add_seconds_local_once(original_timeout, move || {
                        Self::dismiss_popup_static(
                            noti_id,
                            &store3,
                            &widgets2,
                            &timeouts3,
                            &popup_box2,
                            &popup_win2,
                        );
                    });
                    timeouts.borrow_mut().insert(noti_id, sid);
                }
            });

            slot.add_controller(hover_ctrl);
        }

        // Click-to-dismiss (only if close_button_on_hover is OFF — they're mutually exclusive)
        if self.config.click_to_dismiss && !self.config.close_button_on_hover && !noti.acknowledge_to_dismiss {
            if is_popup {
                let click = GestureClick::new();
                let noti_id = noti.id;
                let store = self.store.clone();
                let widgets = self.popup_widgets.clone();
                let timeouts = self.timeout_sources.clone();
                let popup_box = self.popup_box.clone();
                let popup_window = self.popup_window.clone();
                let body_truncated = body_is_truncated.clone();
                let center_window = self.center_window.clone();

                click.connect_released(move |_, _, _, _| {
                    // If body was truncated, open notification center instead of dismissing
                    if *body_truncated.borrow() {
                        center_window.present();
                        return;
                    }
                    Self::dismiss_popup_static(
                        noti_id,
                        &store,
                        &widgets,
                        &timeouts,
                        &popup_box,
                        &popup_window,
                    );
                });
                slot.add_controller(click);
            } else {
                // Center: click to dismiss
                let click = GestureClick::new();
                let noti_id = noti.id;
                let store = self.store.clone();
                let center_widgets = self.center_widgets.clone();
                let center_box = self.center_box.clone();

                click.connect_released(move |_, _, _, _| {
                    Self::dismiss_center_static(noti_id, &store, &center_widgets, &center_box);
                });
                slot.add_controller(click);
            }
        }

        slot
    }

    fn dismiss_popup_static(
        id: u32,
        store: &SharedStore,
        widgets: &Rc<RefCell<HashMap<u32, GtkBox>>>,
        timeouts: &Rc<RefCell<HashMap<u32, glib2::SourceId>>>,
        popup_box: &GtkBox,
        popup_window: &Window,
    ) {
        // Remove timeout if still pending
        if let Some(source_id) = timeouts.borrow_mut().remove(&id) {
            source_id.remove();
        }

        Self::animate_remove_popup_by_id(id, widgets, popup_box, popup_window);

        // Remove from store
        let mut s = store.lock().unwrap();
        s.close(id, CloseReason::Dismissed);
        // Trigger refresh so center side gets cleaned up too
        s.notify_change();
    }

    fn animate_remove_popup_by_id(
        id: u32,
        widgets: &Rc<RefCell<HashMap<u32, GtkBox>>>,
        popup_box: &GtkBox,
        popup_window: &Window,
    ) {
        if let Some(widget) = widgets.borrow_mut().remove(&id) {
            Self::animate_remove_popup_widget(widget, popup_box, popup_window);
        }
    }

    fn animate_remove_popup_widget(widget: GtkBox, popup_box: &GtkBox, popup_window: &Window) {
        Self::animate_widget_opacity(
            widget.clone().upcast::<gtk4::Widget>(),
            1.0,
            0.0,
            Self::POPUP_ANIMATION_MS,
        );

        if let Some(child) = widget.first_child() {
            if let Ok(revealer) = child.downcast::<Revealer>() {
                revealer.set_reveal_child(false);
            }
        }

        let popup_box = popup_box.clone();
        let popup_window = popup_window.clone();
        glib2::timeout_add_local_once(
            std::time::Duration::from_millis(Self::POPUP_ANIMATION_MS as u64),
            move || {
                popup_box.remove(&widget);
                if popup_box.first_child().is_none() {
                    popup_window.set_visible(false);
                }
            },
        );
    }

    fn first_popup_target_height(popup_box: &GtkBox, slot: &GtkBox, width: i32, max_h: i32) -> i32 {
        let (_, base_h, _, _) = popup_box.measure(gtk4::Orientation::Vertical, width);
        let (_, row_h, _, _) = slot.measure(gtk4::Orientation::Vertical, width);
        (base_h + row_h).min(max_h).max(1)
    }

    fn begin_appear_animation(revealer: Revealer, slot_wrapper: GtkBox) {
        revealer.set_reveal_child(true);
        Self::animate_widget_opacity(
            slot_wrapper.upcast::<gtk4::Widget>(),
            0.0,
            1.0,
            Self::POPUP_ANIMATION_MS,
        );
    }

    fn start_appear_after_resize(
        popup_window: Window,
        revealer: Revealer,
        slot_wrapper: GtkBox,
        target_h: i32,
    ) {
        let checks = Rc::new(RefCell::new(0_u8));
        let checks_ref = checks.clone();

        glib2::timeout_add_local(std::time::Duration::from_millis(16), move || {
            let current_h = popup_window.height();
            let mut checks_mut = checks_ref.borrow_mut();
            *checks_mut += 1;

            if current_h >= target_h || *checks_mut >= 30 {
                drop(checks_mut);
                Self::begin_appear_animation(revealer.clone(), slot_wrapper.clone());
                glib2::ControlFlow::Break
            } else {
                glib2::ControlFlow::Continue
            }
        });
    }

    fn animate_widget_opacity(widget: gtk4::Widget, from: f64, to: f64, duration_ms: u32) {
        widget.set_opacity(from);

        if duration_ms == 0 {
            widget.set_opacity(to);
            return;
        }

        let start = std::time::Instant::now();
        let delta = to - from;

        glib2::timeout_add_local(std::time::Duration::from_millis(16), move || {
            let elapsed_ms = start.elapsed().as_millis() as u32;
            let progress = (elapsed_ms as f64 / duration_ms as f64).clamp(0.0, 1.0);
            widget.set_opacity(from + delta * progress);

            if progress >= 1.0 {
                glib2::ControlFlow::Break
            } else {
                glib2::ControlFlow::Continue
            }
        });
    }

    /// Dismiss a notification from the center panel
    fn dismiss_center_static(
        id: u32,
        store: &SharedStore,
        center_widgets: &Rc<RefCell<HashMap<u32, GtkBox>>>,
        center_box: &GtkBox,
    ) {
        if let Some(widget) = center_widgets.borrow_mut().remove(&id) {
            center_box.remove(&widget);
        }

        let mut s = store.lock().unwrap();
        s.close(id, CloseReason::Dismissed);
        // Trigger refresh so popup side gets cleaned up too
        s.notify_change();
    }

    /// Deferred resize of the popup window so it grows/shrinks to fit content.
    fn schedule_popup_resize(&self) {
        let max_h = self.max_popup_h;
        let width = self.config.popup_width;
        let anim_ms = Self::POPUP_ANIMATION_MS;

        let popup_window_early = self.popup_window.clone();
        let popup_box_early = self.popup_box.clone();
        glib2::timeout_add_local_once(std::time::Duration::from_millis(16), move || {
            Self::resize_popup_to_content(&popup_window_early, &popup_box_early, width, max_h);
        });

        let popup_window_final = self.popup_window.clone();
        let popup_box_final = self.popup_box.clone();
        glib2::timeout_add_local_once(
            std::time::Duration::from_millis((anim_ms + 50) as u64),
            move || {
                Self::resize_popup_to_content(&popup_window_final, &popup_box_final, width, max_h);
            },
        );
    }

    fn resize_popup_to_content(popup_window: &Window, popup_box: &GtkBox, width: i32, max_h: i32) {
        if popup_box.first_child().is_none() {
            popup_window.set_visible(false);
            return;
        }

        let (_, natural_h, _, _) = popup_box.measure(gtk4::Orientation::Vertical, width);
        let target_h = natural_h.min(max_h).max(1);

        popup_window.set_default_size(width, target_h);
        popup_window.queue_resize();
    }

    fn projected_popup_height(popup_box: &GtkBox, width: i32, max_h: i32) -> i32 {
        let spacing = popup_box.spacing();
        let mut total_h = 0;
        let mut row_count = 0;

        let mut child = popup_box.first_child();
        while let Some(widget) = child {
            let row_h = if let Ok(wrapper) = widget.clone().downcast::<GtkBox>() {
                if let Some(revealer_widget) = wrapper.first_child() {
                    if let Ok(revealer) = revealer_widget.downcast::<Revealer>() {
                        if let Some(content) = revealer.child() {
                            let (_, natural_h, _, _) =
                                content.measure(gtk4::Orientation::Vertical, width);
                            natural_h.max(1)
                        } else {
                            let (_, natural_h, _, _) =
                                widget.measure(gtk4::Orientation::Vertical, width);
                            natural_h.max(1)
                        }
                    } else {
                        let (_, natural_h, _, _) =
                            widget.measure(gtk4::Orientation::Vertical, width);
                        natural_h.max(1)
                    }
                } else {
                    let (_, natural_h, _, _) =
                        widget.measure(gtk4::Orientation::Vertical, width);
                    natural_h.max(1)
                }
            } else {
                let (_, natural_h, _, _) = widget.measure(gtk4::Orientation::Vertical, width);
                natural_h.max(1)
            };

            if row_count > 0 {
                total_h += spacing;
            }
            total_h += row_h;
            row_count += 1;
            child = widget.next_sibling();
        }

        total_h.min(max_h).max(1)
    }

    /// Build an image widget from ImageData
    fn build_image(&self, image: &ImageData) -> Option<Image> {
        match image {
            ImageData::Raw {
                width,
                height,
                rowstride,
                has_alpha,
                bits_per_sample,
                channels,
                data,
            } => {
                let expected_min_rowstride = width.saturating_mul(*channels);
                if *rowstride < expected_min_rowstride {
                    log::warn!(
                        "Raw image rowstride {} is smaller than width*channels {} ({}x{})",
                        rowstride,
                        expected_min_rowstride,
                        width,
                        channels
                    );
                }

                let pixbuf = Pixbuf::from_bytes(
                    &glib2::Bytes::from(data),
                    gdk4::gdk_pixbuf::Colorspace::Rgb,
                    *has_alpha,
                    *bits_per_sample,
                    *width,
                    *height,
                    *rowstride,
                );
                let texture = gdk4::Texture::for_pixbuf(&pixbuf);
                let img = Image::from_paintable(Some(&texture));
                img.set_pixel_size(48);
                Some(img)
            }
            ImageData::Path(path) => {
                let clean = path.strip_prefix("file://").unwrap_or(path);
                if std::path::Path::new(clean).exists() {
                    let img = Image::from_file(clean);
                    img.set_pixel_size(48);
                    Some(img)
                } else {
                    log::warn!("Image path not found: {}", clean);
                    None
                }
            }
            ImageData::Name(name) => {
                let display = gdk4::Display::default().expect("No display");
                let theme = gtk4::IconTheme::for_display(&display);
                // Prefer symbolic variant for better contrast on dark themes
                let symbolic_name = if !name.ends_with("-symbolic") {
                    format!("{}-symbolic", name)
                } else {
                    name.clone()
                };
                let icon_name = if theme.has_icon(&symbolic_name) {
                    symbolic_name
                } else if theme.has_icon(name) {
                    name.clone()
                } else {
                    log::warn!("Icon not found in theme: {}", name);
                    return None;
                };
                let img = Image::from_icon_name(&icon_name);
                img.set_pixel_size(48);
                Some(img)
            }
            ImageData::None => None,
        }
    }

    /// Add a notification to the center panel
    fn add_to_center(&self, noti: &Notification) {
        let widget = self.build_notification_widget(noti, false, 0);
        // Remove empty placeholder if present
        if let Some(first) = self.center_box.first_child() {
            if first.widget_name() == "center-empty" {
                self.center_box.remove(&first);
            }
        }
        self.center_widgets.borrow_mut().insert(noti.id, widget.clone());
        self.center_box.append(&widget);
    }

    /// Toggle the notification center visibility
    pub fn toggle_center(&self) {
        let visible = self.center_window.is_visible();
        if visible {
            self.center_window.set_visible(false);
            // Re-show popups if there are any remaining
            if !self.popup_widgets.borrow().is_empty() {
                self.popup_window.set_visible(true);
                self.popup_window.present();
            }
        } else {
            // Hide popups while center is open
            self.popup_window.set_visible(false);
            self.center_window.present();
            self.center_window.grab_focus();
        }
    }

    /// Refresh the UI from the store (called after store changes)
    pub fn refresh(&self) {
        // Sync center: remove widgets for notifications no longer in store
        let (store_ids, replaced_ids): (Vec<u32>, Vec<u32>) = {
            let mut store = self.store.lock().unwrap();
            (store.order.clone(), store.take_replaced_ids())
        };

        for id in replaced_ids {
            if let Some(widget) = self.center_widgets.borrow_mut().remove(&id) {
                self.center_box.remove(&widget);
            }

            if let Some(source_id) = self.timeout_sources.borrow_mut().remove(&id) {
                source_id.remove();
            }

            Self::animate_remove_popup_by_id(
                id,
                &self.popup_widgets,
                &self.popup_box,
                &self.popup_window,
            );
        }

        let center_ids: Vec<u32> = self.center_widgets.borrow().keys().cloned().collect();
        for id in center_ids {
            if !store_ids.contains(&id) {
                if let Some(widget) = self.center_widgets.borrow_mut().remove(&id) {
                    self.center_box.remove(&widget);
                }
            }
        }

        // Sync popups: remove widgets for notifications no longer in store
        let popup_ids: Vec<u32> = self.popup_widgets.borrow().keys().cloned().collect();
        for id in popup_ids {
            if !store_ids.contains(&id) {
                if let Some(source_id) = self.timeout_sources.borrow_mut().remove(&id) {
                    source_id.remove();
                }
                Self::animate_remove_popup_by_id(
                    id,
                    &self.popup_widgets,
                    &self.popup_box,
                    &self.popup_window,
                );
            }
        }
        // Hide popup window if empty
        if self.popup_box.first_child().is_none() {
            self.popup_window.set_visible(false);
        }

        // Add new notifications to center (ALL notifications, not just visible_popups)
        let all_notis: Vec<Notification> = {
            let store = self.store.lock().unwrap();
            let existing: Vec<u32> = self.center_widgets.borrow().keys().cloned().collect();
            store
                .all_notifications()
                .into_iter()
                .filter(|n| !existing.contains(&n.id))
                .cloned()
                .collect()
        };
        for noti in all_notis {
            self.add_to_center(&noti);
        }

        // Show empty placeholder if center is now empty
        if self.center_widgets.borrow().is_empty() && self.center_box.first_child().is_none() {
            let empty_label = Label::new(Some("No Notifications"));
            empty_label.set_widget_name("center-empty");
            empty_label.set_css_classes(&["dim-label"]);
            self.center_box.append(&empty_label);
        }

        // Collect new notification IDs that need popup widgets (only visible_popups)
        let new_notis: Vec<Notification> = {
            let store = self.store.lock().unwrap();
            let existing: Vec<u32> = self.popup_widgets.borrow().keys().cloned().collect();
            store
                .visible_popups()
                .into_iter()
                .filter(|n| !existing.contains(&n.id))
                .cloned()
                .collect()
        };

        for noti in new_notis {
            self.show_notification(&noti);
        }

        // Single deferred resize after all additions / removals are done
        if self.popup_box.first_child().is_some() {
            self.schedule_popup_resize();
        }
    }
}

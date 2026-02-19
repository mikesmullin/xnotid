# xnotid

A custom X11 notification daemon built with Rust, GTK4, and zbus. Designed as a replacement for `naughty` (AwesomeWM's built-in notification system).

## Notification Cards

xnotid supports structured "card" notifications when the notification body is JSON with this envelope:

```json
{
  "xnotid_card": "v1",
  "type": "multiple-choice",
  "question": "Which option should I use?",
  "choices": [
    { "id": "a", "label": "Option A" },
    { "id": "b", "label": "Option B" }
  ],
  "allow_other": true
}
```

Supported card types:

- `multiple-choice`
  - fields: `question`, `choices`, optional `allow_other`
  - renders custom choice buttons; `allow_other` adds a text entry + submit
  - selected choice emits `ActionInvoked(id, action_key)`
- `permission`
  - fields: `question`, optional `allow_label` (default: `Allow`)
  - renders a single allow button that emits `ActionInvoked(id, "allow")`

If a card is detected, xnotid treats the notification as acknowledge-to-dismiss.
For normal notifications, existing body/actions behavior is unchanged.

## Building

```sh
cargo build # debug
cargo build --release
```

## Running

```sh
RUST_LOG=info ./target/debug/xnotid
```

## Configuration

On first launch, xnotid writes its default CSS to `~/.config/xnotid/style.css`. Edit that file and restart xnotid to apply changes.

An optional YAML config can be placed at `~/.config/xnotid/config.yaml`:

```yaml
monitor: 0
position_x: "right"
position_y: "top"
popup_width: 400
max_visible: 3
timeout_normal: 10   # seconds, 0 = never
timeout_low: 5
timeout_critical: 0  # 0 = never auto-dismiss
```

## System Configuration Changes

### 1. AwesomeWM — Disable naughty D-Bus listener (`~/.config/awesome/rc.lua`)

Naughty claims `org.freedesktop.Notifications` on the session bus. To let xnotid own that name, stub out `naughty.dbus` **before** `require("naughty")`:

```lua
-- Must preload stub BEFORE requiring naughty, since naughty/init.lua auto-loads
-- naughty.dbus which calls dbus.request_name().
package.loaded["naughty.dbus"] = {}
local naughty = require("naughty")
```

This keeps `naughty.notify()` working for AwesomeWM's own error dialogs, but prevents it from listening on D-Bus.

### 2. AwesomeWM — Window rules (`~/.config/awesome/rc.lua`)

Add a rule for xnotid windows (floating, no titlebar, no border, on-top, positioned top-right on the desired screen. This rule must come **after** the general titlebar rule, and the general rule needs an `except_any` to avoid overriding it:

```lua
-- Add titlebars to normal clients and dialogs
{ rule_any = {type = { "normal", "dialog" }
  }, except_any = { class = { "xnotid" } },
  properties = { titlebars_enabled = true }
},

-- xnotid notification daemon — no titlebar, floating, always on top, screen 1 top-right
{ rule_any = {
    class = { "xnotid" },
    name = { "xnotid-popups", "xnotid-center" },
  }, properties = {
    floating = true,
    ontop = true,
    focusable = true,
    focus = false,
    titlebars_enabled = false,
    border_width = 0,
    skip_taskbar = true,
    sticky = false,
    screen = 1,
  },
  callback = function(c)
    -- Popups should never steal focus; center should accept keyboard input (Esc)
    if c.name == "xnotid-popups" then
      c.focusable = false
    elseif c.name == "xnotid-center" then
      c.focusable = true
    end

    local s = c.screen or screen[1]
    local g = s.workarea
    local margin = 12
    c:geometry({ x = g.x + g.width - c.width - margin, y = g.y + margin })
  end
},
```

Adjust `screen = 1` and the `callback` geometry to place notifications on your preferred monitor/corner.
This keeps popups non-focus-stealing while allowing notification-center keyboard shortcuts (like Esc to close).

### 3. System tray behavior

- Left-clicking the xnotid tray icon toggles the notification center.

You can also toggle it from the command line or a keybinding:

```sh
gdbus call --session --dest org.xnotid.Control \
  --object-path /org/xnotid/Control \
  --method org.xnotid.Control.ToggleCenter
```

### 4. Picom — Disable shadows on xnotid windows (`~/.config/picom.conf`)

Picom draws drop shadows on all windows by default. Add xnotid to the shadow exclusion in the `rules` block:

```conf
rules: (
  # ... existing rules ...
  {
    match = "name = 'Notification'   || "
            "name = 'xnotid-popups'  || "
            "name = 'xnotid-center'  || "
            "class_g = 'Conky'       || "
            "class_g ?= 'Notify-osd' || "
            "class_g = 'Cairo-clock' || "
            "_GTK_FRAME_EXTENTS@";
    shadow = false;
  }
)
```

> **Note:** If your picom.conf uses both old-style options (`shadow-exclude`) and the `rules` block, only `rules` takes effect. Put the exclusion in whichever format your config uses.

## After Setup

1. Restart AwesomeWM: `Mod4+Ctrl+r` or `echo "awesome.restart()" | awesome-client`
2. Restart picom: `pkill picom; picom -b`
3. Launch xnotid: `RUST_LOG=info ./target/debug/xnotid &`
4. Test: `notify-send -i dialog-information "Hello" "It works!"`

## Installation

### 1. Install the binary

```sh
cargo build --release
sudo install -Dm755 target/release/xnotid /usr/local/bin/xnotid
```

### 2. Systemd user service

Use the tracked service file at `xnotid.service` (repo root), then install it to your user systemd directory and enable it:

```sh
install -Dm644 xnotid.service ~/.config/systemd/user/xnotid.service
systemctl --user daemon-reload
systemctl --user enable --now xnotid.service
```

### Updating

```sh
cd /path/to/xnotid/src
git pull
cargo build --release
sudo install -Dm755 target/release/xnotid /usr/local/bin/xnotid
install -Dm644 xnotid.service ~/.config/systemd/user/xnotid.service
systemctl --user daemon-reload
systemctl --user restart xnotid
```

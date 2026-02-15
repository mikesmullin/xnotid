use crate::dbus_server::UiCommand;
use std::collections::BTreeMap;
use std::sync::OnceLock;
use std::sync::mpsc::Sender;
use std::thread;
use std::time::Duration;
use x11rb::COPY_DEPTH_FROM_PARENT;
use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::*;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

const SYSTEM_TRAY_REQUEST_DOCK: u32 = 0;
const XEMBED_MAPPED: u32 = 1;
const ICON_DRAW_SIZE: u16 = 15;
const BELL_BMP_BYTES: &[u8] = include_bytes!("../assets/bell.bmp");

struct IconBatch {
    pixel: u32,
    rects: Vec<Rectangle>,
}

static BELL_BATCHES: OnceLock<Vec<IconBatch>> = OnceLock::new();

struct Atoms {
    manager: Atom,
    system_tray_selection: Atom,
    system_tray_opcode: Atom,
    xembed_info: Atom,
}

fn intern_atom(conn: &RustConnection, name: &str) -> Result<Atom, Box<dyn std::error::Error>> {
    Ok(conn.intern_atom(false, name.as_bytes())?.reply()?.atom)
}

fn init_atoms(
    conn: &RustConnection,
    screen_num: usize,
) -> Result<Atoms, Box<dyn std::error::Error>> {
    Ok(Atoms {
        manager: intern_atom(conn, "MANAGER")?,
        system_tray_selection: intern_atom(conn, &format!("_NET_SYSTEM_TRAY_S{}", screen_num))?,
        system_tray_opcode: intern_atom(conn, "_NET_SYSTEM_TRAY_OPCODE")?,
        xembed_info: intern_atom(conn, "_XEMBED_INFO")?,
    })
}

fn tray_owner(
    conn: &RustConnection,
    selection: Atom,
) -> Result<Window, Box<dyn std::error::Error>> {
    Ok(conn.get_selection_owner(selection)?.reply()?.owner)
}

fn create_tray_window(
    conn: &RustConnection,
    root: Window,
    atoms: &Atoms,
) -> Result<Window, Box<dyn std::error::Error>> {
    let win = conn.generate_id()?;
    let aux = CreateWindowAux::new()
        .background_pixmap(BackPixmap::PARENT_RELATIVE)
        .border_pixel(0)
        .event_mask(
            EventMask::EXPOSURE
                | EventMask::BUTTON_PRESS
                | EventMask::STRUCTURE_NOTIFY
                | EventMask::PROPERTY_CHANGE,
        );

    conn.create_window(
        COPY_DEPTH_FROM_PARENT,
        win,
        root,
        0,
        0,
        ICON_DRAW_SIZE,
        ICON_DRAW_SIZE,
        0,
        WindowClass::INPUT_OUTPUT,
        0,
        &aux,
    )?;

    conn.change_property32(
        PropMode::REPLACE,
        win,
        atoms.xembed_info,
        atoms.xembed_info,
        &[1, XEMBED_MAPPED],
    )?;

    Ok(win)
}

fn dock_to_tray(
    conn: &RustConnection,
    tray_owner: Window,
    dock_window: Window,
    atoms: &Atoms,
) -> Result<(), Box<dyn std::error::Error>> {
    let msg = ClientMessageEvent {
        response_type: CLIENT_MESSAGE_EVENT,
        format: 32,
        sequence: 0,
        window: tray_owner,
        type_: atoms.system_tray_opcode,
        data: ClientMessageData::from([
            x11rb::CURRENT_TIME,
            SYSTEM_TRAY_REQUEST_DOCK,
            dock_window,
            0,
            0,
        ]),
    };

    conn.send_event(false, tray_owner, EventMask::NO_EVENT, msg)?;
    conn.map_window(dock_window)?;
    conn.flush()?;
    Ok(())
}

fn draw_icon(
    conn: &RustConnection,
    win: Window,
    colormap: Colormap,
    _black_pixel: u32,
) -> Result<(), Box<dyn std::error::Error>> {
    conn.clear_area(false, win, 0, 0, ICON_DRAW_SIZE, ICON_DRAW_SIZE)?;

    let bell_batches = BELL_BATCHES.get_or_init(|| load_bell_batches_from_embedded(conn, colormap));
    for batch in bell_batches {
        let fg_gc = conn.generate_id()?;
        conn.create_gc(
            fg_gc,
            win,
            &CreateGCAux::new()
                .foreground(batch.pixel)
                .background(batch.pixel),
        )?;
        conn.poly_fill_rectangle(win, fg_gc, &batch.rects)?;
        conn.free_gc(fg_gc)?;
    }

    conn.flush()?;
    Ok(())
}

fn read_u16_le(bytes: &[u8], off: usize) -> Option<u16> {
    let s = bytes.get(off..off + 2)?;
    Some(u16::from_le_bytes([s[0], s[1]]))
}

fn read_u32_le(bytes: &[u8], off: usize) -> Option<u32> {
    let s = bytes.get(off..off + 4)?;
    Some(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn read_i32_le(bytes: &[u8], off: usize) -> Option<i32> {
    let s = bytes.get(off..off + 4)?;
    Some(i32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn try_load_indexed_bmp_batches(conn: &RustConnection, colormap: Colormap) -> Option<Vec<IconBatch>> {
    let data = BELL_BMP_BYTES;
    if data.len() < 54 || data.get(0..2)? != b"BM" {
        return None;
    }

    let pixel_offset = read_u32_le(data, 10)? as usize;
    let dib_size = read_u32_le(data, 14)? as usize;
    if dib_size < 40 {
        return None;
    }

    let width_i = read_i32_le(data, 18)?;
    let height_i = read_i32_le(data, 22)?;
    let planes = read_u16_le(data, 26)?;
    let bpp = read_u16_le(data, 28)?;
    let compression = read_u32_le(data, 30)?;
    let colors_used = read_u32_le(data, 46)? as usize;

    if planes != 1 || compression != 0 || !matches!(bpp, 1 | 4 | 8) {
        return None;
    }

    let src_w = width_i.unsigned_abs() as usize;
    let src_h = height_i.unsigned_abs() as usize;
    if src_w == 0 || src_h == 0 {
        return None;
    }

    let palette_start = 14 + dib_size;
    if pixel_offset <= palette_start || pixel_offset > data.len() {
        return None;
    }

    let max_palette_entries = (pixel_offset - palette_start) / 4;
    let default_entries = 1usize << (bpp as usize);
    let palette_entries = if colors_used == 0 {
        default_entries.min(max_palette_entries)
    } else {
        colors_used.min(max_palette_entries)
    };
    if palette_entries == 0 {
        return None;
    }

    let row_stride = (((src_w * bpp as usize) + 31) / 32) * 4;
    let bitmap_len = row_stride.checked_mul(src_h)?;
    if pixel_offset + bitmap_len > data.len() {
        return None;
    }

    let mut grouped = BTreeMap::<u32, Vec<Rectangle>>::new();
    let dst_w = ICON_DRAW_SIZE as usize;
    let dst_h = ICON_DRAW_SIZE as usize;

    for y in 0..dst_h {
        for x in 0..dst_w {
            let src_x = x * src_w / dst_w;
            let src_y = y * src_h / dst_h;
            let row = if height_i > 0 {
                src_h - 1 - src_y
            } else {
                src_y
            };

            let row_start = pixel_offset + row * row_stride;
            let idx = match bpp {
                8 => *data.get(row_start + src_x)? as usize,
                4 => {
                    let b = *data.get(row_start + (src_x / 2))?;
                    if src_x % 2 == 0 {
                        ((b >> 4) & 0x0f) as usize
                    } else {
                        (b & 0x0f) as usize
                    }
                }
                1 => {
                    let b = *data.get(row_start + (src_x / 8))?;
                    let bit = 7 - (src_x % 8);
                    ((b >> bit) & 0x01) as usize
                }
                _ => return None,
            };

            // Requested behavior: palette index 0 is transparent.
            if idx == 0 || idx >= palette_entries {
                continue;
            }

            let p = palette_start + idx * 4;
            let b = *data.get(p)?;
            let g = *data.get(p + 1)?;
            let r = *data.get(p + 2)?;
            let key = ((r as u32) << 16) | ((g as u32) << 8) | (b as u32);
            grouped.entry(key).or_default().push(Rectangle {
                x: x as i16,
                y: y as i16,
                width: 1,
                height: 1,
            });
        }
    }

    if grouped.is_empty() {
        return None;
    }

    let mut batches = Vec::<IconBatch>::new();
    for (key, rects) in grouped {
        let r = ((key >> 16) & 0xff) as u16;
        let g = ((key >> 8) & 0xff) as u16;
        let b = (key & 0xff) as u16;
        if let Ok(cookie) = conn.alloc_color(colormap, r * 257, g * 257, b * 257) {
            if let Ok(reply) = cookie.reply() {
                batches.push(IconBatch {
                    pixel: reply.pixel,
                    rects,
                });
            }
        }
    }

    if batches.is_empty() {
        return None;
    }

    log::info!(
        "Loaded indexed embedded tray icon assets/bell.bmp ({}x{}, {}bpp, idx0 transparent)",
        src_w,
        src_h,
        bpp
    );
    Some(batches)
}

fn load_bell_batches_from_embedded(conn: &RustConnection, colormap: Colormap) -> Vec<IconBatch> {
    if let Some(batches) = try_load_indexed_bmp_batches(conn, colormap) {
        return batches;
    }

    log::warn!("Falling back to built-in bell icon; indexed embedded BMP parse failed");

    let bell_bitmap = [
        "0000100000",
        "0001110000",
        "0011111000",
        "0111111100",
        "0111111100",
        "1111111110",
        "1111111110",
        "0111111100",
        "0011111000",
        "0001110000",
    ];

    let mut bell = Vec::<Rectangle>::new();
    for (row, line) in bell_bitmap.iter().enumerate() {
        for (col, c) in line.chars().enumerate() {
            if c == '1' {
                bell.push(Rectangle {
                    x: 4 + col as i16,
                    y: 2 + row as i16,
                    width: 1,
                    height: 1,
                });
            }
        }
    }

    bell.push(Rectangle {
        x: 6,
        y: 12,
        width: 6,
        height: 1,
    });
    bell.push(Rectangle {
        x: 8,
        y: 14,
        width: 2,
        height: 1,
    });

    let fallback_pixel = conn
        .alloc_color(colormap, 0xffff, 0xffff, 0xffff)
        .ok()
        .and_then(|c| c.reply().ok())
        .map(|r| r.pixel)
        .unwrap_or(0xffffff);

    vec![IconBatch {
        pixel: fallback_pixel,
        rects: bell,
    }]
}

fn run_xembed_tray(cmd_tx: Sender<UiCommand>) -> Result<(), Box<dyn std::error::Error>> {
    let (conn, screen_num) = x11rb::connect(None)?;
    let screen = &conn.setup().roots[screen_num];
    let root = screen.root;
    let colormap = screen.default_colormap;
    let black_pixel = screen.black_pixel;

    let atoms = init_atoms(&conn, screen_num)?;

    let owner = loop {
        match tray_owner(&conn, atoms.system_tray_selection) {
            Ok(o) if o != x11rb::NONE => break o,
            Ok(_) => thread::sleep(Duration::from_millis(500)),
            Err(err) => {
                log::warn!("fallback tray owner query failed: {}", err);
                thread::sleep(Duration::from_millis(500));
            }
        }
    };

    let dock_window = create_tray_window(&conn, root, &atoms)?;
    dock_to_tray(&conn, owner, dock_window, &atoms)?;
    draw_icon(&conn, dock_window, colormap, black_pixel)?;
    log::info!("XEmbed tray icon docked");

    loop {
        match conn.wait_for_event() {
            Ok(Event::Expose(_)) => {
                let _ = draw_icon(&conn, dock_window, colormap, black_pixel);
            }
            Ok(Event::ButtonPress(_)) => {
                let _ = cmd_tx.send(UiCommand::ToggleCenter);
            }
            Ok(Event::ClientMessage(event)) => {
                if event.type_ == atoms.manager {
                    let data = event.data.as_data32();
                    if data[1] == atoms.system_tray_selection {
                        let _ = tray_owner(&conn, atoms.system_tray_selection)
                            .and_then(|new_owner| dock_to_tray(&conn, new_owner, dock_window, &atoms));
                    }
                }
            }
            Ok(Event::DestroyNotify(_)) => break,
            Ok(_) => {}
            Err(err) => return Err(Box::new(err)),
        }
    }

    Ok(())
}

pub fn start_tray_service(cmd_tx: Sender<UiCommand>) {
    thread::spawn(move || {
        if let Err(err) = run_xembed_tray(cmd_tx) {
            log::warn!("xembed tray exited with error: {}", err);
        }
    });
}

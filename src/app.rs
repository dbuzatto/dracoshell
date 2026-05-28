use std::collections::HashMap;
use std::sync::Arc;

use alacritty_terminal::event::{Event as TermEvent, WindowSize};
use alacritty_terminal::grid::Scroll;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::dpi::PhysicalPosition;
use winit::event_loop::{ActiveEventLoop, EventLoopProxy};
use winit::keyboard::{Key, KeyCode, ModifiersState, NamedKey, PhysicalKey};
#[cfg(target_os = "linux")]
use winit::platform::wayland::WindowAttributesExtWayland;
#[cfg(target_os = "linux")]
use winit::platform::x11::WindowAttributesExtX11;
use winit::window::{Window, WindowId};

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point};

use crate::config::Config;
use crate::layout::{Direction, Layout, PaneId, Rect, Split};
use crate::renderer::{PaneView, Renderer, TabBarInfo, TAB_WIDTH, PANE_PADDING};
use crate::terminal::Terminal;
use crate::UserEvent;

const APP_ID: &str = "dracoshell";

/// Height of the tab bar in pixels. Bar is hidden when there's only one tab.
pub const TAB_BAR_HEIGHT: f32 = 28.0;

// ── Mouse text selection ──────────────────────────────────────────────────

struct SelectionState {
    pane_id: PaneId,
    anchor_col: usize,
    anchor_row: usize,
    cur_col: usize,
    cur_row: usize,
}

impl SelectionState {
    /// Returns (col1, row1, col2, row2) with start always before end.
    fn normalized(&self) -> (usize, usize, usize, usize) {
        if (self.anchor_row, self.anchor_col) <= (self.cur_row, self.cur_col) {
            (self.anchor_col, self.anchor_row, self.cur_col, self.cur_row)
        } else {
            (self.cur_col, self.cur_row, self.anchor_col, self.anchor_row)
        }
    }
}

// ── First-run onboard wizard ──────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq)]
enum OnboardStep { FontSize, AccentColor }

struct OnboardState {
    step:       OnboardStep,
    font_size:  f32,
    accent_buf: String,
    accent_rgb: Option<(u8, u8, u8)>,
}

impl OnboardState {
    fn new() -> Self {
        Self {
            step:       OnboardStep::FontSize,
            font_size:  14.0,
            accent_buf: String::new(),
            accent_rgb: Some((0xff, 0x2a, 0x2a)), // default red swatch
        }
    }

    fn parse_rgb(buf: &str) -> Option<(u8, u8, u8)> {
        let hex = buf.trim_start_matches('#');
        if hex.len() != 6 { return None; }
        let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
        let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
        let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
        Some((r, g, b))
    }

    fn effective_accent(&self) -> String {
        if self.accent_buf.is_empty() {
            "#FF2A2A".to_string()
        } else {
            format!("#{}", self.accent_buf.trim_start_matches('#').to_uppercase())
        }
    }
}

const ICON_BYTES: &[u8] = include_bytes!("../assets/dracoshell.png");

/// On macOS, winit's window icon does not appear in the Dock. The Dock icon
/// requires an explicit NSApplication.setApplicationIconImage: call.
/// We also apply a rounded-corner (squircle) mask so the icon matches the
/// macOS icon shape used by all native apps.
#[cfg(target_os = "macos")]
fn set_dock_icon() {
    use objc::runtime::{Class, Object};
    use objc::{msg_send, sel, sel_impl};

    let png = rounded_icon_png().unwrap_or_else(|| ICON_BYTES.to_vec());

    unsafe {
        let data_cls = match Class::get("NSData") {
            Some(c) => c,
            None => return,
        };
        let data: *mut Object =
            msg_send![data_cls, dataWithBytes:png.as_ptr() length:png.len()];

        let img_cls = match Class::get("NSImage") {
            Some(c) => c,
            None => return,
        };
        let img: *mut Object = msg_send![img_cls, alloc];
        let img: *mut Object = msg_send![img, initWithData: data];
        if img.is_null() {
            log::warn!("could not create NSImage for Dock icon");
            return;
        }

        let app_cls = match Class::get("NSApplication") {
            Some(c) => c,
            None => return,
        };
        let app: *mut Object = msg_send![app_cls, sharedApplication];
        let _: () = msg_send![app, setApplicationIconImage: img];
    }
}

/// Load the bundled PNG and apply a rounded-corner mask that approximates
/// the macOS squircle shape (corner radius ≈ 22.5 % of the smaller dimension).
/// Pixels outside the shape get alpha = 0; edge pixels are anti-aliased over
/// a 2-pixel band to avoid jaggies.
#[cfg(target_os = "macos")]
fn rounded_icon_png() -> Option<Vec<u8>> {
    let img = image::load_from_memory(ICON_BYTES).ok()?;
    let mut rgba = img.to_rgba8();
    let (w, h) = (rgba.width() as i32, rgba.height() as i32);
    let r = (w.min(h) as f32 * 0.2247) as i32;

    for y in 0..h {
        for x in 0..w {
            // Only the four corner regions need masking.
            if !(x < r || x >= w - r) || !(y < r || y >= h - r) {
                continue;
            }
            let cx = if x < r { r } else { w - r - 1 };
            let cy = if y < r { r } else { h - r - 1 };
            let dist = (((x - cx) * (x - cx) + (y - cy) * (y - cy)) as f32).sqrt();
            let rf = r as f32;
            if dist >= rf + 1.0 {
                rgba.get_pixel_mut(x as u32, y as u32).0[3] = 0;
            } else if dist > rf - 1.0 {
                // 2-pixel anti-aliased edge
                let a = ((rf + 1.0 - dist) * 0.5 * 255.0) as u8;
                let current = rgba.get_pixel(x as u32, y as u32).0[3];
                rgba.get_pixel_mut(x as u32, y as u32).0[3] = a.min(current);
            }
        }
    }

    let mut cursor = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(rgba)
        .write_to(&mut cursor, image::ImageFormat::Png)
        .ok()?;
    Some(cursor.into_inner())
}

fn load_icon() -> Option<winit::window::Icon> {
    let img = match image::load_from_memory(ICON_BYTES) {
        Ok(i) => i.to_rgba8(),
        Err(e) => {
            log::warn!("decode icon: {e}");
            return None;
        }
    };
    let (w, h) = (img.width(), img.height());
    match winit::window::Icon::from_rgba(img.into_raw(), w, h) {
        Ok(i) => Some(i),
        Err(e) => {
            log::warn!("icon from rgba: {e}");
            None
        }
    }
}

/// One tab = one independent BSP tree of terminal panes. Switching tabs
/// keeps each tab's panes alive in the background.
struct Tab {
    layout: Layout,
    panes: HashMap<PaneId, Terminal>,
}

impl Tab {
    fn new(initial: PaneId, terminal: Terminal) -> Self {
        let mut panes = HashMap::new();
        panes.insert(initial, terminal);
        Self {
            layout: Layout::new(initial),
            panes,
        }
    }
}

pub struct App {
    proxy: EventLoopProxy<UserEvent>,
    config: Config,
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    tabs: Vec<Tab>,
    active_tab: usize,
    next_pane_id: PaneId,
    modifiers: ModifiersState,
    cursor: PhysicalPosition<f64>,
    onboard: Option<OnboardState>,
    clipboard: Option<arboard::Clipboard>,
    selection: Option<SelectionState>,
    is_selecting: bool,
}

impl App {
    pub fn new(proxy: EventLoopProxy<UserEvent>, config: Config) -> Self {
        Self {
            proxy,
            config,
            window: None,
            renderer: None,
            tabs: Vec::new(),
            active_tab: 0,
            next_pane_id: 1,
            modifiers: ModifiersState::empty(),
            cursor: PhysicalPosition::new(0.0, 0.0),
            onboard: None,
            clipboard: arboard::Clipboard::new().ok(),
            selection: None,
            is_selecting: false,
        }
    }

    // ── Onboard wizard ────────────────────────────────────────────────────

    fn handle_onboard_key(&mut self, event: &KeyEvent, event_loop: &ActiveEventLoop) {
        // Copy step so we can release the immutable borrow before taking &mut.
        let step = match self.onboard.as_ref() {
            Some(o) => o.step,
            None    => return,
        };

        match step {
            OnboardStep::FontSize => match &event.logical_key {
                Key::Named(NamedKey::ArrowUp) => {
                    let new_size = { let o = self.onboard.as_mut().unwrap(); o.font_size = (o.font_size + 1.0).min(48.0); o.font_size };
                    if let Some(r) = self.renderer.as_mut() { r.set_font_size(new_size).ok(); }
                    self.request_redraw();
                }
                Key::Named(NamedKey::ArrowDown) => {
                    let new_size = { let o = self.onboard.as_mut().unwrap(); o.font_size = (o.font_size - 1.0).max(6.0); o.font_size };
                    if let Some(r) = self.renderer.as_mut() { r.set_font_size(new_size).ok(); }
                    self.request_redraw();
                }
                Key::Named(NamedKey::Enter) => {
                    self.onboard.as_mut().unwrap().step = OnboardStep::AccentColor;
                    self.request_redraw();
                }
                Key::Named(NamedKey::Escape) => event_loop.exit(),
                Key::Character(s) if s.as_str() == "c" && self.modifiers.control_key() => event_loop.exit(),
                _ => {}
            },

            OnboardStep::AccentColor => match &event.logical_key {
                Key::Named(NamedKey::Enter) => self.finish_onboard(event_loop),
                Key::Named(NamedKey::Backspace) => {
                    let o = self.onboard.as_mut().unwrap();
                    o.accent_buf.pop();
                    o.accent_rgb = if o.accent_buf.is_empty() {
                        Some((0xff, 0x2a, 0x2a))
                    } else {
                        OnboardState::parse_rgb(&o.accent_buf)
                    };
                    self.request_redraw();
                }
                Key::Named(NamedKey::Escape) => event_loop.exit(),
                Key::Character(s) if s.as_str() == "c" && self.modifiers.control_key() => event_loop.exit(),
                Key::Character(s) => {
                    if let Some(c) = s.chars().next() {
                        if c.is_ascii_hexdigit() || c == '#' {
                            let o = self.onboard.as_mut().unwrap();
                            o.accent_buf.push(c);
                            o.accent_rgb = OnboardState::parse_rgb(&o.accent_buf);
                            self.request_redraw();
                        }
                    }
                }
                _ => {}
            },
        }
    }

    fn finish_onboard(&mut self, event_loop: &ActiveEventLoop) {
        let ob = match self.onboard.take() {
            Some(o) => o,
            None    => return,
        };
        let accent = ob.effective_accent();
        if let Err(e) = crate::config::write_custom(ob.font_size, &accent) {
            log::error!("write config: {e}");
        }
        // Spawn the real shell now that config is saved
        let pane_id = self.alloc_pane_id();
        let viewport = self.pane_viewport();
        let size = match self.renderer.as_ref() {
            Some(r) => Self::rect_to_term_size(r, viewport),
            None    => { event_loop.exit(); return; }
        };
        match Terminal::new(self.proxy.clone(), pane_id, size, None) {
            Ok(term) => {
                self.tabs.push(Tab::new(pane_id, term));
                self.active_tab = 0;
                self.reflow();
                self.request_redraw();
            }
            Err(e) => { log::error!("spawn terminal: {e}"); event_loop.exit(); }
        }
    }

    fn alloc_pane_id(&mut self) -> PaneId {
        let id = self.next_pane_id;
        self.next_pane_id += 1;
        id
    }

    fn pane_viewport(&self) -> Rect {
        let phys = self
            .window
            .as_ref()
            .map(|w| w.inner_size())
            .unwrap_or_default();
        let tab_h = if self.tabs.len() > 1 { TAB_BAR_HEIGHT } else { 0.0 };
        Rect {
            x: 0.0,
            y: tab_h,
            w: phys.width as f32,
            h: (phys.height as f32 - tab_h).max(1.0),
        }
    }

    fn rect_to_term_size(renderer: &Renderer, rect: Rect) -> WindowSize {
        let (cols, lines) = renderer.grid_dims_for(rect.w, rect.h);
        let (cw, ch) = renderer.cell_size();
        WindowSize {
            num_cols: cols as u16,
            num_lines: lines as u16,
            cell_width: cw.round() as u16,
            cell_height: ch.round() as u16,
        }
    }

    /// Recompute layout for the active tab and resize each pane to its rect.
    fn reflow(&mut self) {
        let viewport = self.pane_viewport();
        let active = self.active_tab;
        let Some(renderer) = self.renderer.as_ref() else {
            return;
        };
        let Some(tab) = self.tabs.get_mut(active) else {
            return;
        };
        let rects = tab.layout.compute(viewport);
        for (id, rect) in &rects {
            if let Some(term) = tab.panes.get_mut(id) {
                let size = Self::rect_to_term_size(renderer, *rect);
                term.resize_to(size);
            }
        }
    }

    fn split_focused(&mut self, split: Split) {
        let Some(tab) = self.tabs.get_mut(self.active_tab) else {
            return;
        };
        let new_id = {
            let id = self.next_pane_id;
            self.next_pane_id += 1;
            id
        };
        tab.layout.split_focused(new_id, split);
        let provisional = WindowSize {
            num_cols: 80,
            num_lines: 24,
            cell_width: 8,
            cell_height: 18,
        };
        match Terminal::new(self.proxy.clone(), new_id, provisional, None) {
            Ok(term) => {
                tab.panes.insert(new_id, term);
                self.reflow();
                self.request_redraw();
            }
            Err(e) => log::error!("spawn pane failed: {e:?}"),
        }
    }

    fn close_focused(&mut self, event_loop: &ActiveEventLoop) {
        let Some(tab) = self.tabs.get_mut(self.active_tab) else {
            return;
        };
        let focused = tab.layout.focused();
        let only_pane = !tab.layout.close_focused();
        if only_pane {
            // Last pane in this tab → close the tab itself.
            tab.panes.remove(&focused);
            if let Some(renderer) = self.renderer.as_mut() {
                renderer.forget_pane(focused);
            }
            self.close_tab(event_loop);
            return;
        }
        tab.panes.remove(&focused);
        if let Some(renderer) = self.renderer.as_mut() {
            renderer.forget_pane(focused);
        }
        self.reflow();
        self.request_redraw();
    }

    fn focus_dir(&mut self, dir: Direction) {
        let viewport = self.pane_viewport();
        if let Some(tab) = self.tabs.get_mut(self.active_tab) {
            tab.layout.focus_neighbor(dir, viewport);
            self.request_redraw();
        }
    }

    fn new_tab(&mut self) {
        let pane_id = self.alloc_pane_id();
        let viewport = {
            let phys = self
                .window
                .as_ref()
                .map(|w| w.inner_size())
                .unwrap_or_default();
            // Assume tab bar will be visible after we add this one.
            let tab_h = TAB_BAR_HEIGHT;
            Rect {
                x: 0.0,
                y: tab_h,
                w: phys.width as f32,
                h: (phys.height as f32 - tab_h).max(1.0),
            }
        };
        let Some(renderer) = self.renderer.as_ref() else {
            return;
        };
        let size = Self::rect_to_term_size(renderer, viewport);
        let terminal = match Terminal::new(self.proxy.clone(), pane_id, size, None) {
            Ok(t) => t,
            Err(e) => {
                log::error!("spawn pane failed: {e:?}");
                return;
            }
        };
        let tab = Tab::new(pane_id, terminal);
        self.tabs.push(tab);
        self.active_tab = self.tabs.len() - 1;
        // Going from 1 to 2 tabs reveals the tab bar — reflow ALL tabs to
        // respect the new vertical budget.
        self.reflow_all();
        self.request_redraw();
    }

    fn close_tab(&mut self, event_loop: &ActiveEventLoop) {
        if self.tabs.is_empty() {
            event_loop.exit();
            return;
        }
        self.tabs.remove(self.active_tab);
        if self.tabs.is_empty() {
            event_loop.exit();
            return;
        }
        if self.active_tab >= self.tabs.len() {
            self.active_tab = self.tabs.len() - 1;
        }
        self.reflow_all();
        self.request_redraw();
    }

    fn switch_tab(&mut self, idx: usize) {
        if idx < self.tabs.len() && idx != self.active_tab {
            self.active_tab = idx;
            self.request_redraw();
        }
    }

    fn cycle_tab(&mut self, forward: bool) {
        if self.tabs.len() < 2 {
            return;
        }
        let n = self.tabs.len();
        self.active_tab = if forward {
            (self.active_tab + 1) % n
        } else {
            (self.active_tab + n - 1) % n
        };
        self.request_redraw();
    }

    fn reflow_all(&mut self) {
        let (Some(renderer), phys) = (
            self.renderer.as_ref(),
            self.window
                .as_ref()
                .map(|w| w.inner_size())
                .unwrap_or_default(),
        ) else {
            return;
        };
        let tab_h = if self.tabs.len() > 1 { TAB_BAR_HEIGHT } else { 0.0 };
        let viewport = Rect {
            x: 0.0,
            y: tab_h,
            w: phys.width as f32,
            h: (phys.height as f32 - tab_h).max(1.0),
        };
        for tab in self.tabs.iter_mut() {
            let rects = tab.layout.compute(viewport);
            for (id, rect) in &rects {
                if let Some(term) = tab.panes.get_mut(id) {
                    let size = Self::rect_to_term_size(renderer, *rect);
                    term.resize_to(size);
                }
            }
        }
    }

    fn request_redraw(&self) {
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    fn scroll_focused(&mut self, scroll: Scroll) {
        let Some(tab) = self.tabs.get(self.active_tab) else {
            return;
        };
        let Some(term) = tab.panes.get(&tab.layout.focused()) else {
            return;
        };
        let mut t = term.term.lock();
        t.scroll_display(scroll);
        drop(t);
        self.request_redraw();
    }

    fn find_tab_with_pane(&self, pane: PaneId) -> Option<usize> {
        self.tabs.iter().position(|t| t.panes.contains_key(&pane))
    }

    fn paste_from_clipboard(&mut self) {
        let text = match self.clipboard.as_mut().and_then(|cb| cb.get_text().ok()) {
            Some(t) => t,
            None => return,
        };
        if let Some(tab) = self.tabs.get(self.active_tab) {
            if let Some(term) = tab.panes.get(&tab.layout.focused()) {
                term.send_bytes(text.into_bytes());
            }
        }
    }

    /// Convert a physical pixel position to (pane_id, col, row) in screen cell coords.
    fn pixel_to_cell(&self, pos: PhysicalPosition<f64>) -> Option<(PaneId, usize, usize)> {
        let renderer = self.renderer.as_ref()?;
        let (cw, ch) = renderer.cell_size();
        let tab = self.tabs.get(self.active_tab)?;
        let viewport = self.pane_viewport();
        let rects = tab.layout.compute(viewport);
        for (id, rect) in &rects {
            if pos.x >= rect.x as f64
                && pos.x < (rect.x + rect.w) as f64
                && pos.y >= rect.y as f64
                && pos.y < (rect.y + rect.h) as f64
            {
                let rel_x = (pos.x as f32 - rect.x - PANE_PADDING).max(0.0);
                let rel_y = (pos.y as f32 - rect.y - PANE_PADDING).max(0.0);
                let col = (rel_x / cw) as usize;
                let row = (rel_y / ch) as usize;
                return Some((*id, col, row));
            }
        }
        None
    }

    /// Extract selected text from the terminal grid (screen-coord selection).
    fn extract_selection_text(&self, sel: &SelectionState) -> String {
        let tab = match self.tabs.get(self.active_tab) {
            Some(t) => t,
            None => return String::new(),
        };
        let term_arc = match tab.panes.get(&sel.pane_id) {
            Some(t) => &t.term,
            None => return String::new(),
        };
        let (c1, r1, c2, r2) = sel.normalized();
        let t = term_arc.lock();
        let grid = t.grid();
        let cols = grid.columns();
        let display_offset = grid.display_offset() as i32;
        let mut text = String::new();
        for row in r1..=r2 {
            let line = Line(row as i32 - display_offset);
            let sc1 = if row == r1 { c1 } else { 0 };
            let sc2 = if row == r2 { c2 } else { cols.saturating_sub(1) };
            let mut line_text = String::new();
            for col in sc1..=sc2.min(cols.saturating_sub(1)) {
                let cell = &grid[Point::new(line, Column(col))];
                line_text.push(if cell.c == '\0' { ' ' } else { cell.c });
            }
            let trimmed = line_text.trim_end_matches(' ');
            text.push_str(trimmed);
            if row < r2 {
                text.push('\n');
            }
        }
        text
    }

    fn copy_selection(&mut self) {
        let text = match &self.selection {
            Some(sel) => self.extract_selection_text(sel),
            None => return,
        };
        if text.is_empty() { return; }
        if let Some(cb) = self.clipboard.as_mut() {
            cb.set_text(text).ok();
        }
    }

    fn try_shortcut(&mut self, event: &KeyEvent, event_loop: &ActiveEventLoop) -> bool {
        let mods = self.modifiers;

        // Copy / Paste:
        //   macOS  → Cmd+C (copy selection), Cmd+V (paste)
        //   Linux  → Ctrl+Shift+C / Ctrl+Shift+V
        #[cfg(target_os = "macos")]
        if mods.super_key() && !mods.control_key() && !mods.alt_key() && event.state == ElementState::Pressed {
            match event.physical_key {
                PhysicalKey::Code(KeyCode::KeyV) => { self.paste_from_clipboard(); return true; }
                PhysicalKey::Code(KeyCode::KeyC) => { self.copy_selection(); return true; }
                _ => {}
            }
        }
        #[cfg(not(target_os = "macos"))]
        if mods.control_key() && mods.shift_key() && !mods.alt_key() && event.state == ElementState::Pressed {
            match event.physical_key {
                PhysicalKey::Code(KeyCode::KeyV) => { self.paste_from_clipboard(); return true; }
                PhysicalKey::Code(KeyCode::KeyC) => { self.copy_selection(); return true; }
                _ => {}
            }
        }

        // Shift+PageUp/PageDown → scrollback navigation.
        if mods.shift_key() && !mods.control_key() && !mods.alt_key() {
            if event.state == ElementState::Pressed {
                if let Key::Named(NamedKey::PageUp) = event.logical_key {
                    self.scroll_focused(Scroll::PageUp);
                    return true;
                }
                if let Key::Named(NamedKey::PageDown) = event.logical_key {
                    self.scroll_focused(Scroll::PageDown);
                    return true;
                }
            }
        }

        // Tabs use Ctrl+Shift to follow the de-facto terminal convention
        // (kitty, alacritty, gnome-terminal) and avoid clashing with KDE's
        // global Ctrl+Alt+T → Konsole shortcut.
        if mods.control_key() && mods.shift_key() && !mods.alt_key() {
            if event.state != ElementState::Pressed {
                return true;
            }
            match &event.logical_key {
                Key::Character(s) => {
                    let lower = s.as_str().to_ascii_lowercase();
                    match lower.as_str() {
                        "t" => {
                            self.new_tab();
                            return true;
                        }
                        d if d.len() == 1
                            && d.chars().next().unwrap().is_ascii_digit() =>
                        {
                            let digit = d.chars().next().unwrap().to_digit(10).unwrap() as usize;
                            if digit >= 1 && digit <= 9 {
                                self.switch_tab(digit - 1);
                            }
                            return true;
                        }
                        _ => {}
                    }
                }
                Key::Named(NamedKey::Tab) => {
                    self.cycle_tab(true);
                    return true;
                }
                _ => {}
            }
        }

        // Pane management (splits, focus, close, quit) — Ctrl+Alt on all
        // platforms. On macOS, Option+letter produces special characters via
        // dead-key composition (e.g. Option+H → ˇ), so we match the physical
        // key code there. On Linux/Windows the logical key works correctly.
        if !(mods.control_key() && mods.alt_key()) {
            return false;
        }
        if event.state != ElementState::Pressed {
            return true;
        }
        #[cfg(target_os = "macos")]
        {
            match event.physical_key {
                PhysicalKey::Code(KeyCode::KeyH) => {
                    self.split_focused(Split::Horizontal);
                    return true;
                }
                PhysicalKey::Code(KeyCode::KeyV) => {
                    self.split_focused(Split::Vertical);
                    return true;
                }
                PhysicalKey::Code(KeyCode::KeyW) => {
                    self.close_focused(event_loop);
                    return true;
                }
                PhysicalKey::Code(KeyCode::KeyQ) => {
                    event_loop.exit();
                    return true;
                }
                PhysicalKey::Code(KeyCode::ArrowLeft) => {
                    self.focus_dir(Direction::Left);
                    return true;
                }
                PhysicalKey::Code(KeyCode::ArrowRight) => {
                    self.focus_dir(Direction::Right);
                    return true;
                }
                PhysicalKey::Code(KeyCode::ArrowUp) => {
                    self.focus_dir(Direction::Up);
                    return true;
                }
                PhysicalKey::Code(KeyCode::ArrowDown) => {
                    self.focus_dir(Direction::Down);
                    return true;
                }
                _ => return false,
            }
        }
        #[cfg(not(target_os = "macos"))]
        match &event.logical_key {
            Key::Character(s) => {
                let lower = s.as_str().to_ascii_lowercase();
                match lower.as_str() {
                    "h" => {
                        self.split_focused(Split::Horizontal);
                        true
                    }
                    "v" => {
                        self.split_focused(Split::Vertical);
                        true
                    }
                    "w" => {
                        self.close_focused(event_loop);
                        true
                    }
                    "q" => {
                        event_loop.exit();
                        true
                    }
                    _ => false,
                }
            }
            Key::Named(NamedKey::ArrowLeft) => {
                self.focus_dir(Direction::Left);
                true
            }
            Key::Named(NamedKey::ArrowRight) => {
                self.focus_dir(Direction::Right);
                true
            }
            Key::Named(NamedKey::ArrowUp) => {
                self.focus_dir(Direction::Up);
                true
            }
            Key::Named(NamedKey::ArrowDown) => {
                self.focus_dir(Direction::Down);
                true
            }
            _ => false,
        }
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let mut attrs = Window::default_attributes()
            .with_title("dracoshell")
            .with_window_icon(load_icon())
            .with_inner_size(winit::dpi::LogicalSize::new(
                self.config.window.width as f32,
                self.config.window.height as f32,
            ));
        #[cfg(target_os = "linux")]
        {
            attrs = WindowAttributesExtWayland::with_name(attrs, APP_ID, "");
            attrs = WindowAttributesExtX11::with_name(attrs, APP_ID, APP_ID);
        }
        let window = Arc::new(
            event_loop
                .create_window(attrs)
                .expect("failed to create window"),
        );

        #[cfg(target_os = "macos")]
        set_dock_icon();

        let renderer = pollster::block_on(Renderer::new(window.clone(), &self.config))
            .expect("failed to initialize renderer");

        self.window   = Some(window);
        self.renderer = Some(renderer);

        if !crate::config::exists() {
            // Enter first-run wizard — render directly, no PTY yet.
            self.onboard = Some(OnboardState::new());
            self.request_redraw();
        } else {
            let phys = self.window.as_ref().unwrap().inner_size();
            let initial_rect = Rect { x: 0.0, y: 0.0, w: phys.width as f32, h: phys.height as f32 };
            let term_size = Self::rect_to_term_size(self.renderer.as_ref().unwrap(), initial_rect);
            let pane_id  = self.alloc_pane_id();
            let terminal = Terminal::new(self.proxy.clone(), pane_id, term_size, None)
                .expect("failed to spawn terminal");
            self.tabs.push(Tab::new(pane_id, terminal));
            self.active_tab = 0;
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        let UserEvent::Term { pane, event: ev } = event;
        match ev {
            TermEvent::Wakeup => {
                // Only redraw if this pane is in the currently active tab.
                if let Some(tab_idx) = self.find_tab_with_pane(pane) {
                    if tab_idx == self.active_tab {
                        self.request_redraw();
                    }
                }
            }
            TermEvent::Title(t) => {
                if let Some(tab_idx) = self.find_tab_with_pane(pane) {
                    if tab_idx == self.active_tab {
                        if let Some(tab) = self.tabs.get(self.active_tab) {
                            if tab.layout.focused() == pane {
                                if let Some(w) = &self.window {
                                    w.set_title(&format!("dracoshell — {t}"));
                                }
                            }
                        }
                    }
                }
            }
            TermEvent::ResetTitle => {
                if let Some(w) = &self.window {
                    w.set_title("dracoshell");
                }
            }
            TermEvent::Exit | TermEvent::ChildExit(_) => {
                let Some(tab_idx) = self.find_tab_with_pane(pane) else {
                    return;
                };
                // Switch to that tab first so close_focused targets correctly.
                let saved_active = self.active_tab;
                self.active_tab = tab_idx;
                if let Some(tab) = self.tabs.get_mut(tab_idx) {
                    tab.layout.set_focused(pane);
                }
                self.close_focused(event_loop);
                if !self.tabs.is_empty() {
                    self.active_tab = saved_active.min(self.tabs.len() - 1);
                }
            }
            _ => {}
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(_) => {
                if let Some(renderer) = self.renderer.as_mut() {
                    if let Some(w) = &self.window {
                        renderer.resize(w.inner_size());
                    }
                }
                self.reflow_all();
                self.request_redraw();
            }
            WindowEvent::ModifiersChanged(mods) => {
                self.modifiers = mods.state();
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let lines = match delta {
                    MouseScrollDelta::LineDelta(_, y) => (y * 3.0) as i32,
                    MouseScrollDelta::PixelDelta(p) => (p.y / 18.0) as i32,
                };
                if lines != 0 {
                    self.scroll_focused(Scroll::Delta(lines));
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = position;
                if self.is_selecting {
                    if let Some((_pid, col, row)) = self.pixel_to_cell(position) {
                        if let Some(sel) = self.selection.as_mut() {
                            sel.cur_col = col;
                            sel.cur_row = row;
                            self.request_redraw();
                        }
                    }
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                if button == MouseButton::Left {
                    match state {
                        ElementState::Pressed => {
                            // Tab bar click.
                            if self.tabs.len() > 1 && self.cursor.y < TAB_BAR_HEIGHT as f64 {
                                let idx = (self.cursor.x / TAB_WIDTH as f64) as usize;
                                if idx < self.tabs.len() {
                                    self.switch_tab(idx);
                                }
                                return;
                            }
                            // Start selection.
                            self.selection = None;
                            if let Some((pane_id, col, row)) = self.pixel_to_cell(self.cursor) {
                                self.selection = Some(SelectionState {
                                    pane_id,
                                    anchor_col: col,
                                    anchor_row: row,
                                    cur_col: col,
                                    cur_row: row,
                                });
                                self.is_selecting = true;
                                // Focus the pane the user clicked.
                                if let Some(tab) = self.tabs.get_mut(self.active_tab) {
                                    tab.layout.set_focused(pane_id);
                                }
                            }
                        }
                        ElementState::Released => {
                            self.is_selecting = false;
                            // Clear single-cell (zero-area) selections.
                            if let Some(sel) = &self.selection {
                                let (c1, r1, c2, r2) = sel.normalized();
                                if c1 == c2 && r1 == r2 {
                                    self.selection = None;
                                }
                            }
                            self.request_redraw();
                        }
                    }
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                // In onboard mode all keys are consumed by the wizard.
                if self.onboard.is_some() {
                    if event.state == ElementState::Pressed {
                        self.handle_onboard_key(&event, event_loop);
                    }
                    return;
                }
                if self.try_shortcut(&event, event_loop) {
                    return;
                }
                if let Some(bytes) = encode_key(&event, self.modifiers) {
                    self.selection = None; // typing clears selection
                    if let Some(tab) = self.tabs.get(self.active_tab) {
                        if let Some(term) = tab.panes.get(&tab.layout.focused()) {
                            term.send_bytes(bytes);
                        }
                    }
                }
            }
            WindowEvent::RedrawRequested => {
                // Onboard wizard renders the full window itself.
                if let Some(ob) = &self.onboard {
                    if let Some(renderer) = self.renderer.as_mut() {
                        if let Err(e) = renderer.render_onboard(
                            ob.font_size,
                            &ob.accent_buf,
                            ob.accent_rgb,
                            ob.step == OnboardStep::AccentColor,
                        ) {
                            log::error!("onboard render: {e}");
                        }
                    }
                    return;
                }

                let (Some(renderer), Some(tab)) =
                    (self.renderer.as_mut(), self.tabs.get(self.active_tab))
                else {
                    return;
                };
                let (w, h) = renderer.surface_size();
                let tab_h = if self.tabs.len() > 1 { TAB_BAR_HEIGHT } else { 0.0 };
                let viewport = Rect {
                    x: 0.0,
                    y: tab_h,
                    w: w as f32,
                    h: (h as f32 - tab_h).max(1.0),
                };
                let rects = tab.layout.compute(viewport);
                let focused = tab.layout.focused();
                let views: Vec<PaneView<'_>> = rects
                    .iter()
                    .filter_map(|(id, rect)| {
                        tab.panes.get(id).map(|term| {
                            let selection = self.selection.as_ref()
                                .filter(|s| s.pane_id == *id)
                                .map(|s| s.normalized());
                            PaneView {
                                id: *id,
                                term: &term.term,
                                rect: *rect,
                                focused: *id == focused,
                                selection,
                            }
                        })
                    })
                    .collect();
                let tab_bar = if self.tabs.len() > 1 {
                    Some(TabBarInfo {
                        count: self.tabs.len(),
                        active: self.active_tab,
                        height: TAB_BAR_HEIGHT,
                        width: w as f32,
                    })
                } else {
                    None
                };
                if let Err(e) = renderer.render(&views, tab_bar) {
                    log::error!("render error: {e:?}");
                }
            }
            _ => {}
        }
    }
}

fn encode_key(event: &KeyEvent, mods: ModifiersState) -> Option<Vec<u8>> {
    if event.state != ElementState::Pressed {
        return None;
    }

    if mods.control_key() && !mods.alt_key() && !mods.super_key() {
        if let Key::Character(s) = &event.logical_key {
            if let Some(c) = s.chars().next() {
                let lower = c.to_ascii_lowercase();
                let byte = match lower {
                    'a'..='z' => Some((lower as u8) - b'a' + 1),
                    ' ' | '@' => Some(0),
                    '[' => Some(0x1b),
                    '\\' => Some(0x1c),
                    ']' => Some(0x1d),
                    '^' => Some(0x1e),
                    '_' | '?' => Some(0x1f),
                    _ => None,
                };
                if let Some(b) = byte {
                    return Some(vec![b]);
                }
            }
        }
    }

    let alt_prefix: &[u8] = if mods.alt_key() { b"\x1b" } else { b"" };

    let bytes: Vec<u8> = match &event.logical_key {
        Key::Named(NamedKey::Enter) => b"\r".to_vec(),
        Key::Named(NamedKey::Backspace) => b"\x7f".to_vec(),
        Key::Named(NamedKey::Tab) => b"\t".to_vec(),
        Key::Named(NamedKey::Escape) => b"\x1b".to_vec(),
        Key::Named(NamedKey::ArrowUp) => b"\x1b[A".to_vec(),
        Key::Named(NamedKey::ArrowDown) => b"\x1b[B".to_vec(),
        Key::Named(NamedKey::ArrowRight) => b"\x1b[C".to_vec(),
        Key::Named(NamedKey::ArrowLeft) => b"\x1b[D".to_vec(),
        Key::Named(NamedKey::Home) => b"\x1b[H".to_vec(),
        Key::Named(NamedKey::End) => b"\x1b[F".to_vec(),
        Key::Named(NamedKey::Delete) => b"\x1b[3~".to_vec(),
        Key::Named(NamedKey::PageUp) => b"\x1b[5~".to_vec(),
        Key::Named(NamedKey::PageDown) => b"\x1b[6~".to_vec(),
        _ => {
            let mut v = Vec::new();
            v.extend_from_slice(alt_prefix);
            v.extend_from_slice(event.text.as_ref()?.as_bytes());
            return Some(v);
        }
    };
    Some(bytes)
}

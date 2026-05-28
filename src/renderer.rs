use std::sync::Arc;
use std::time::Instant;

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::Term;
use anyhow::{Context, Result};
use winit::dpi::PhysicalSize;
use winit::window::Window;

use crate::colors::{self, srgb_byte_to_linear, Color};
use crate::config::Config;
use crate::layout::{PaneId, Rect};
use crate::quads::QuadRenderer;
use crate::terminal::EventProxy;
use crate::text::{CellMetrics, TextRenderer};

const FONT_SIZE: f32 = 14.0;
const PANE_PADDING: f32 = 10.0;
const BORDER_WIDTH: f32 = 1.5;

/// Bundled font so the binary works regardless of the host's font config.
const FONT_BYTES: &[u8] = include_bytes!("../assets/Hack-Regular.ttf");

// Background color comes from the active theme; converted to linear below.

const DRACO_RED: [f32; 4] = [1.0, 0.16, 0.16, 1.0];
const CURSOR_RED_RGBA: [f32; 4] = [1.0, 0.16, 0.16, 0.45];
const SCROLLBAR_WIDTH: f32 = 4.0;
const SCROLLBAR_COLOR: [f32; 4] = [1.0, 0.16, 0.16, 0.7];

pub struct PaneView<'a> {
    pub id: PaneId,
    pub term: &'a Arc<FairMutex<Term<EventProxy>>>,
    pub rect: Rect,
    pub focused: bool,
}

/// Top-bar describing the open tabs. `None` is passed when there's only one
/// tab — the bar is hidden to give panes more vertical space.
pub struct TabBarInfo {
    pub count: usize,
    pub active: usize,
    pub height: f32,
    pub width: f32,
}

const TAB_BG_INACTIVE: [f32; 4] = [0.13, 0.14, 0.16, 1.0];
const TAB_BG_ACTIVE: [f32; 4] = [1.0, 0.16, 0.16, 1.0];
const TAB_BAR_BG: [f32; 4] = [0.08, 0.08, 0.10, 1.0];
pub const TAB_WIDTH: f32 = 60.0;
const TAB_LABEL_FG_ACTIVE: Color = Color::rgb(0xff, 0xff, 0xff);
const TAB_LABEL_FG_INACTIVE: Color = Color::rgb(0xab, 0xb2, 0xbf);

struct PaneSnapshot {
    rows: Vec<Vec<(char, Color)>>,
    cursor_col: usize,
    cursor_line: usize,
    /// True when the user has scrolled into history — cursor should be hidden.
    scrolled: bool,
    /// Lines of scrollback available behind the visible region.
    history_size: usize,
    /// How far we are scrolled back, in lines.
    display_offset: usize,
    /// Number of visible lines (screen height in cells).
    screen_lines: usize,
}

pub struct Renderer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    text: TextRenderer,
    /// Quads drawn UNDER text (tab bar background, future cell backgrounds).
    bg_quads: QuadRenderer,
    /// Quads drawn OVER text (cursor block, focus border, scrollbar).
    fg_quads: QuadRenderer,
    metrics: CellMetrics,
}

impl Renderer {
    pub async fn new(window: Arc<Window>, _app_config: &Config) -> Result<Self> {
        let size = window.inner_size();
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY,
            ..Default::default()
        });
        let surface = instance.create_surface(window).context("create surface")?;

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .context("no suitable GPU adapter")?;

        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("dracoshell device"),
                    required_features: wgpu::Features::empty(),
                    required_limits: wgpu::Limits::default(),
                    memory_hints: wgpu::MemoryHints::Performance,
                },
                None,
            )
            .await
            .context("request device")?;

        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(caps.formats[0]);

        let max_dim = device.limits().max_texture_dimension_2d;
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.clamp(1, max_dim),
            height: size.height.clamp(1, max_dim),
            present_mode: caps.present_modes[0],
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        let text = TextRenderer::new(&device, &queue, format, FONT_BYTES, FONT_SIZE)?;
        let bg_quads = QuadRenderer::new(&device, format);
        let fg_quads = QuadRenderer::new(&device, format);
        let metrics = text.metrics();

        Ok(Self {
            surface,
            device,
            queue,
            config,
            text,
            bg_quads,
            fg_quads,
            metrics,
        })
    }

    pub fn surface_size(&self) -> (u32, u32) {
        (self.config.width, self.config.height)
    }

    pub fn cell_size(&self) -> (f32, f32) {
        (self.metrics.cell_w, self.metrics.cell_h)
    }

    pub fn grid_dims_for(&self, w: f32, h: f32) -> (usize, usize) {
        let usable_w = (w - PANE_PADDING * 2.0).max(1.0);
        let usable_h = (h - PANE_PADDING * 2.0).max(1.0);
        let cols = (usable_w / self.metrics.cell_w) as usize;
        let lines = (usable_h / self.metrics.cell_h) as usize;
        (cols.max(1), lines.max(1))
    }

    pub fn resize(&mut self, size: PhysicalSize<u32>) {
        if size.width == 0 || size.height == 0 {
            return;
        }
        let max_dim = self.device.limits().max_texture_dimension_2d;
        self.config.width = size.width.min(max_dim);
        self.config.height = size.height.min(max_dim);
        self.surface.configure(&self.device, &self.config);
    }

    pub fn forget_pane(&mut self, _id: PaneId) {
        // No per-pane state held in this renderer anymore.
    }

    /// Rebuild the glyph atlas at a new font size. Called by the onboard
    /// wizard when the user presses ↑/↓ to preview sizes in real time.
    pub fn set_font_size(&mut self, size: f32) -> Result<()> {
        self.text = TextRenderer::new(
            &self.device,
            &self.queue,
            self.config.format,
            FONT_BYTES,
            size.clamp(6.0, 48.0),
        )?;
        self.metrics = self.text.metrics();
        Ok(())
    }

    /// Render the first-run setup wizard directly — no PTY/terminal grid.
    /// The entire window (banner + prompts) is rendered at the current font
    /// size so pressing ↑/↓ makes everything visually scale in real time.
    pub fn render_onboard(
        &mut self,
        font_size: f32,
        accent_buf: &str,
        accent_rgb: Option<(u8, u8, u8)>,
        on_accent_step: bool,
    ) -> Result<()> {
        const BANNER: &[&str] = &[
            "       __                          __         ____",
            "  ____/ /________ _________  _____/ /_  ___  / / /",
            " / __  / ___/ __ `/ ___/ __ \\/ ___/ __ \\/ _ \\/ / /",
            "/ /_/ / /  / /_/ / /__/ /_/ (__  ) / / /  __/ / /",
            "\\__,_/_/   \\__,_/\\___/\\____/____/_/ /_/\\___/_/_/",
            "",
            "       tiling terminal for Unix · v0.1.0",
        ];

        let w = self.config.width as f32;
        let m = self.metrics;
        let ox = m.cell_w;       // left margin: 1 cell
        let oy = m.cell_h * 2.0; // top margin: 2 cells

        let red   = colors::Color::rgb(0xff, 0x2a, 0x2a);
        let white = colors::Color::rgb(0xdc, 0xdf, 0xe4);
        let dim   = colors::Color::rgb(0x61, 0x67, 0x74);

        self.text.begin();
        self.bg_quads.begin();
        self.fg_quads.begin();

        // ── Banner ────────────────────────────────────────────────────────
        for (row, line) in BANNER.iter().enumerate() {
            let color = if row < 5 { red } else { dim };
            for (col, c) in line.chars().enumerate() {
                self.text.push_cell(&self.queue, c, col as u32, row as u32, [ox, oy], color);
            }
        }

        let after_banner = BANNER.len() as u32 + 1;

        // ── Subtitle + hint ───────────────────────────────────────────────
        let subtitle = "  Welcome to dracoshell. Quick first-run setup:";
        let hint     = "  (Enter to confirm  \u{b7}  Ctrl-C to quit)";
        for (col, c) in subtitle.chars().enumerate() {
            self.text.push_cell(&self.queue, c, col as u32, after_banner, [ox, oy], white);
        }
        for (col, c) in hint.chars().enumerate() {
            self.text.push_cell(&self.queue, c, col as u32, after_banner + 1, [ox, oy], dim);
        }

        let prompt_base = after_banner + 3;

        // ── Font size ─────────────────────────────────────────────────────
        let size_line = format!("  Font size: {:.0}   \u{2191}\u{2193} to adjust", font_size);
        let fg_size = if !on_accent_step { white } else { dim };
        for (col, c) in size_line.chars().enumerate() {
            self.text.push_cell(&self.queue, c, col as u32, prompt_base, [ox, oy], fg_size);
        }

        // ── Accent color ──────────────────────────────────────────────────
        let accent_show  = if accent_buf.is_empty() { "#FF2A2A" } else { accent_buf };
        let color_line   = format!("  Accent color: {}   ", accent_show);
        let col_count    = color_line.chars().count() as u32;
        let fg_color     = if on_accent_step { white } else { dim };
        for (col, c) in color_line.chars().enumerate() {
            self.text.push_cell(&self.queue, c, col as u32, prompt_base + 2, [ox, oy], fg_color);
        }

        // Color swatch next to label
        let (sr, sg, sb) = accent_rgb.unwrap_or((0xff, 0x2a, 0x2a));
        let swatch_x = ox + col_count as f32 * m.cell_w;
        let swatch_y = oy + (prompt_base + 2) as f32 * m.cell_h + m.cell_h * 0.1;
        self.bg_quads.quad(
            swatch_x, swatch_y,
            m.cell_w * 6.0, m.cell_h * 0.8,
            [sr as f32 / 255.0, sg as f32 / 255.0, sb as f32 / 255.0, 1.0],
        );

        // Active-row underline
        let active_row = if on_accent_step { prompt_base + 2 } else { prompt_base };
        let ul_y = oy + active_row as f32 * m.cell_h + m.cell_h - 2.0;
        self.fg_quads.quad(ox, ul_y, w - ox * 2.0, 2.0, DRACO_RED);

        // ── Submit frame ──────────────────────────────────────────────────
        let frame = self.surface.get_current_texture()?;
        let view  = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("dracoshell onboard encoder"),
        });
        let resolution = [w, self.config.height as f32];
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("dracoshell onboard pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(clear_color()),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            self.bg_quads.flush(&self.device, &self.queue, resolution, &mut pass);
            self.text.flush(&self.device, &self.queue, resolution, &mut pass);
            self.fg_quads.flush(&self.device, &self.queue, resolution, &mut pass);
        }
        self.queue.submit(Some(encoder.finish()));
        frame.present();
        Ok(())
    }

    pub fn render(
        &mut self,
        panes: &[PaneView<'_>],
        tab_bar: Option<TabBarInfo>,
    ) -> Result<()> {
        let start = Instant::now();
        let snaps: Vec<PaneSnapshot> = panes.iter().map(|p| snapshot(p.term)).collect();
        let t_snap = start.elapsed();

        // Build glyph instances — one per visible non-blank cell.
        let t0 = Instant::now();
        self.text.begin();
        for (pane, snap) in panes.iter().zip(snaps.iter()) {
            let origin = [
                (pane.rect.x + PANE_PADDING).round(),
                (pane.rect.y + PANE_PADDING).round(),
            ];
            for (row_idx, row) in snap.rows.iter().enumerate() {
                for (col_idx, (c, fg)) in row.iter().enumerate() {
                    self.text.push_cell(
                        &self.queue,
                        *c,
                        col_idx as u32,
                        row_idx as u32,
                        origin,
                        *fg,
                    );
                }
            }
        }
        // Tab bar labels — push tab numbers as text glyphs at their bar slots.
        if let Some(bar) = &tab_bar {
            for i in 0..bar.count {
                let label = format!("{}", i + 1);
                let tab_x = i as f32 * TAB_WIDTH;
                let metrics = self.text.metrics();
                // Center the digit inside the tab cell horizontally + vertically.
                let label_w = label.chars().count() as f32 * metrics.cell_w;
                let label_x = (tab_x + (TAB_WIDTH - label_w) * 0.5).round();
                let label_y = ((bar.height - metrics.cell_h) * 0.5).round();
                let color = if i == bar.active {
                    TAB_LABEL_FG_ACTIVE
                } else {
                    TAB_LABEL_FG_INACTIVE
                };
                for (j, c) in label.chars().enumerate() {
                    self.text.push_cell(
                        &self.queue,
                        c,
                        j as u32,
                        0,
                        [label_x, label_y],
                        color,
                    );
                }
            }
        }
        let t_text = t0.elapsed();

        // Background quads (drawn under text).
        self.bg_quads.begin();
        if let Some(bar) = &tab_bar {
            self.bg_quads
                .quad(0.0, 0.0, bar.width, bar.height, TAB_BAR_BG);
            for i in 0..bar.count {
                let tab_x = i as f32 * TAB_WIDTH;
                let bg = if i == bar.active {
                    TAB_BG_ACTIVE
                } else {
                    TAB_BG_INACTIVE
                };
                self.bg_quads
                    .quad(tab_x + 2.0, 4.0, TAB_WIDTH - 4.0, bar.height - 6.0, bg);
            }
        }

        // Overlay quads (cursor + focus borders + scrollbar).
        let many = panes.len() > 1;
        self.fg_quads.begin();
        for (pane, snap) in panes.iter().zip(snaps.iter()) {
            if pane.focused {
                // Cursor is hidden while scrolled into history — that's how
                // other terminals behave (the cursor really is off-screen).
                if !snap.scrolled {
                    let origin = [
                        (pane.rect.x + PANE_PADDING).round(),
                        (pane.rect.y + PANE_PADDING).round(),
                    ];
                    let cursor_x = origin[0] + snap.cursor_col as f32 * self.metrics.cell_w;
                    let cursor_y = origin[1] + snap.cursor_line as f32 * self.metrics.cell_h;
                    self.fg_quads.quad(
                        cursor_x,
                        cursor_y,
                        self.metrics.cell_w,
                        self.metrics.cell_h,
                        CURSOR_RED_RGBA,
                    );
                }
                if many {
                    self.fg_quads.border(
                        pane.rect.x,
                        pane.rect.y,
                        pane.rect.w,
                        pane.rect.h,
                        BORDER_WIDTH,
                        DRACO_RED,
                    );
                }
            }
            // Scrollbar — only when scrolled. Drawn for every pane that has
            // scrollback shown.
            let _ = snap.scrolled; // kept for clarity
            if snap.history_size > 0 && snap.scrolled {
                let total = (snap.screen_lines + snap.history_size) as f32;
                let viewport = snap.screen_lines as f32;
                let track_h = (pane.rect.h - PANE_PADDING * 2.0).max(1.0);
                let bar_h = (track_h * viewport / total).max(20.0).min(track_h);
                let history_lines = snap.history_size as f32;
                // 0 offset → bar at bottom; max offset → bar at top.
                let from_bottom = snap.display_offset as f32 / history_lines.max(1.0);
                let bar_y = pane.rect.y + PANE_PADDING + (track_h - bar_h) * (1.0 - from_bottom);
                let bar_x = pane.rect.x + pane.rect.w - SCROLLBAR_WIDTH - 2.0;
                self.fg_quads.quad(
                    bar_x,
                    bar_y,
                    SCROLLBAR_WIDTH,
                    bar_h,
                    SCROLLBAR_COLOR,
                );
            }
        }

        let frame = self.surface.get_current_texture()?;
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("dracoshell encoder"),
            });
        let resolution = [self.config.width as f32, self.config.height as f32];
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("dracoshell main pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(clear_color()),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            // Order matters: bg → text → fg so labels sit on top of tab
            // backgrounds and cursors sit on top of glyphs.
            self.bg_quads
                .flush(&self.device, &self.queue, resolution, &mut pass);
            self.text
                .flush(&self.device, &self.queue, resolution, &mut pass);
            self.fg_quads
                .flush(&self.device, &self.queue, resolution, &mut pass);
        }

        let t_submit_start = Instant::now();
        self.queue.submit(Some(encoder.finish()));
        let t_submit = t_submit_start.elapsed();

        let t_present_start = Instant::now();
        frame.present();
        let t_present = t_present_start.elapsed();

        let elapsed = start.elapsed();
        if elapsed.as_millis() > 16 {
            log::info!(
                "slow frame: {:.1}ms ({} panes) — snap {:.1} text {:.1} submit {:.1} present {:.1}",
                ms(elapsed),
                panes.len(),
                ms(t_snap),
                ms(t_text),
                ms(t_submit),
                ms(t_present),
            );
        }
        Ok(())
    }
}

fn clear_color() -> wgpu::Color {
    let bg = colors::bg_default();
    wgpu::Color {
        r: srgb_byte_to_linear(bg.r) as f64,
        g: srgb_byte_to_linear(bg.g) as f64,
        b: srgb_byte_to_linear(bg.b) as f64,
        a: 1.0,
    }
}

fn snapshot(term: &Arc<FairMutex<Term<EventProxy>>>) -> PaneSnapshot {
    let t = term.lock();
    let grid = t.grid();
    let cols = grid.columns();
    let lines = grid.screen_lines();
    // When the user scrolls back through history, display_offset > 0 and the
    // visible window shifts: row 0 of the screen now shows Line(-offset).
    let display_offset = grid.display_offset() as i32;
    let mut rows: Vec<Vec<(char, Color)>> = Vec::with_capacity(lines);
    for line_idx in 0..lines {
        let mut row = Vec::with_capacity(cols);
        let line = Line(line_idx as i32 - display_offset);
        for col_idx in 0..cols {
            let cell = &grid[Point::new(line, Column(col_idx))];
            let c = if cell.c == '\0' { ' ' } else { cell.c };
            let fg = colors::term_color(cell.fg);
            row.push((c, fg));
        }
        rows.push(row);
    }
    let cursor = grid.cursor.point;
    PaneSnapshot {
        rows,
        cursor_col: cursor.column.0,
        cursor_line: cursor.line.0.max(0) as usize,
        scrolled: display_offset > 0,
        history_size: grid.history_size(),
        display_offset: display_offset as usize,
        screen_lines: lines,
    }
}

fn ms(d: std::time::Duration) -> f32 {
    d.as_secs_f32() * 1000.0
}

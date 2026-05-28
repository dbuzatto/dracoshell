mod app;
mod colors;
mod config;
mod layout;
mod quads;
mod renderer;
mod terminal;
mod text;
mod themes;

use std::env;
use std::io::{self, Write};
use std::os::unix::process::CommandExt;

use anyhow::{Context, Result};
use winit::event_loop::{ControlFlow, EventLoop};

use crate::layout::PaneId;

/// ASCII banner shown by `dracoshell --setup`. Designed to fit comfortably in
/// an 80-column terminal.
const SETUP_BANNER: &str = r#"
       __                          __         ____
  ____/ /________ _________  _____/ /_  ___  / / /
 / __  / ___/ __ `/ ___/ __ \/ ___/ __ \/ _ \/ / /
/ /_/ / /  / /_/ / /__/ /_/ (__  ) / / /  __/ / /
\__,_/_/   \__,_/\___/\____/____/_/ /_/\___/_/_/

       tiling terminal for Unix · v0.1.0
"#;

#[derive(Debug)]
pub enum UserEvent {
    Term {
        pane: PaneId,
        event: alacritty_terminal::event::Event,
    },
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(
        "info,wgpu_core=warn,wgpu_hal=warn,naga=warn",
    ))
    .init();

    let args: Vec<String> = env::args().collect();
    if args.iter().any(|a| a == "--setup") {
        return run_setup();
    }
    if args.iter().any(|a| a == "--themes") {
        return run_themes();
    }
    if args.iter().any(|a| a == "--onboard") {
        return run_onboard();
    }
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("dracoshell {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_help();
        return Ok(());
    }

    alacritty_terminal::tty::setup_env();

    let cfg = config::load();
    themes::init(cfg.colors.theme.as_deref());

    let event_loop = EventLoop::<UserEvent>::with_user_event().build()?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let proxy = event_loop.create_proxy();

    let mut app = app::App::new(proxy, cfg);
    event_loop.run_app(&mut app)?;
    Ok(())
}

fn run_themes() -> Result<()> {
    println!("{}", SETUP_BANNER);
    println!("Available themes:");
    println!();
    for (i, t) in themes::THEMES.iter().enumerate() {
        println!("  {:>2}) {}", i + 1, t.display);
    }
    println!();
    print!("Choose [1-{}] (or Enter to cancel): ", themes::THEMES.len());
    io::stdout().flush().ok();
    let mut input = String::new();
    io::stdin().read_line(&mut input).context("read stdin")?;
    let trimmed = input.trim();
    if trimmed.is_empty() {
        println!("Cancelled.");
        return Ok(());
    }
    let idx: usize = match trimmed.parse() {
        Ok(n) if n >= 1 && n <= themes::THEMES.len() => n,
        _ => {
            println!("Invalid selection.");
            return Ok(());
        }
    };
    let theme = &themes::THEMES[idx - 1];
    let path = config::update_theme(theme.name)?;
    println!();
    println!("  Applied: {}", theme.display);
    println!("  Saved to: {}", path.display());
    println!("  Restart dracoshell to see the change.");
    Ok(())
}

fn run_onboard() -> Result<()> {
    println!("{}", SETUP_BANNER);
    println!("  Welcome to dracoshell. Quick first-run setup:");
    println!("  (Backspace to edit · Enter to confirm · Ctrl-C to quit)");
    println!();

    crossterm::terminal::enable_raw_mode().context("enable raw mode")?;
    let res = onboard_prompts();
    crossterm::terminal::disable_raw_mode().ok();

    let (font_size, accent) = res?;

    let path = config::write_custom(font_size, &accent)?;
    println!();
    println!("  Saved config to: {}", path.display());
    println!("  Launching your shell…");
    println!();

    let shell = env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
    let err = std::process::Command::new(&shell).exec();
    eprintln!("failed to exec {shell}: {err}");
    std::process::exit(1);
}

fn onboard_prompts() -> Result<(f32, String)> {
    use crossterm::{
        cursor,
        event::{self, Event, KeyCode, KeyModifiers},
        execute,
        style::{Color as XtColor, Print, ResetColor, SetBackgroundColor},
        terminal::{self, ClearType},
    };

    let mut out = io::stdout();

    // ── Font size (arrow key navigation) ─────────────────────────────────
    const SIZE_MIN: f32 = 6.0;
    const SIZE_MAX: f32 = 48.0;
    const SIZE_DEFAULT: f32 = 14.0;
    let mut font_size = SIZE_DEFAULT;

    loop {
        execute!(out, cursor::MoveToColumn(0), terminal::Clear(ClearType::CurrentLine))?;
        let n = ((font_size - SIZE_MIN) / (SIZE_MAX - SIZE_MIN) * 22.0).round() as usize + 1;
        let bar: String = "█".repeat(n.min(22));
        execute!(
            out,
            Print(format!(
                "  Font size: {:.0}  {}  ↑↓ to change · Enter to confirm",
                font_size, bar
            )),
        )?;
        io::stdout().flush().ok();

        if let Event::Key(k) = event::read()? {
            match k.code {
                KeyCode::Enter => {
                    execute!(out, Print("\r\n"))?;
                    break;
                }
                KeyCode::Up => font_size = (font_size + 1.0).min(SIZE_MAX),
                KeyCode::Down => font_size = (font_size - 1.0).max(SIZE_MIN),
                KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                    crossterm::terminal::disable_raw_mode().ok();
                    std::process::exit(0);
                }
                _ => {}
            }
        }
    }

    execute!(out, Print("\r\n"))?;

    // ── Accent color ──────────────────────────────────────────────────────
    const DEFAULT_ACCENT: &str = "#FF2A2A";
    let mut cbuf = String::new();

    loop {
        execute!(out, cursor::MoveToColumn(0), terminal::Clear(ClearType::CurrentLine))?;
        let raw = if cbuf.is_empty() { DEFAULT_ACCENT } else { &cbuf };
        let hex = raw.trim_start_matches('#');

        execute!(out, Print(format!("  Accent color [{}]: {}", DEFAULT_ACCENT, cbuf)))?;

        if hex.len() == 6 {
            match (
                u8::from_str_radix(&hex[0..2], 16),
                u8::from_str_radix(&hex[2..4], 16),
                u8::from_str_radix(&hex[4..6], 16),
            ) {
                (Ok(r), Ok(g), Ok(b)) => execute!(
                    out,
                    Print("  "),
                    SetBackgroundColor(XtColor::Rgb { r, g, b }),
                    Print("      "),
                    ResetColor,
                )?,
                _ => execute!(out, Print("  ✗ invalid"))?,
            }
        } else if !hex.is_empty() {
            execute!(out, Print("  …"))?;
        }

        io::stdout().flush().ok();

        if let Event::Key(k) = event::read()? {
            match k.code {
                KeyCode::Enter => {
                    execute!(out, Print("\r\n"))?;
                    break;
                }
                KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                    crossterm::terminal::disable_raw_mode().ok();
                    std::process::exit(0);
                }
                KeyCode::Backspace => { cbuf.pop(); }
                KeyCode::Char(c) if c.is_ascii_hexdigit() || c == '#' => { cbuf.push(c); }
                _ => {}
            }
        }
    }

    let accent = if cbuf.is_empty() {
        DEFAULT_ACCENT.to_string()
    } else {
        format!("#{}", cbuf.trim_start_matches('#').to_uppercase())
    };

    Ok((font_size, accent))
}

fn prompt_with_default(label: &str, default: &str) -> Result<String> {
    print!("{label} [{default}]: ");
    io::stdout().flush().ok();
    let mut buf = String::new();
    io::stdin().read_line(&mut buf).context("read stdin")?;
    let trimmed = buf.trim();
    Ok(if trimmed.is_empty() {
        default.to_string()
    } else {
        trimmed.to_string()
    })
}

fn run_setup() -> Result<()> {
    println!("{}", SETUP_BANNER);
    let path = config::config_path().context("could not resolve user config dir")?;
    if path.exists() {
        println!("Config already exists at: {}", path.display());
        println!("Edit it directly, or delete it and re-run `dracoshell --setup`.");
        return Ok(());
    }
    let written = config::write_default()?;
    println!("Created default config at: {}", written.display());
    println!();
    println!("Open the file in your editor of choice and tweak font/colors.");
    println!("Then launch `dracoshell` to start the terminal.");
    Ok(())
}

fn print_help() {
    println!("dracoshell — tiling terminal for Unix");
    println!();
    println!("USAGE:");
    println!("    dracoshell           Launch the terminal");
    println!("    dracoshell --setup   Print banner and write default config");
    println!("    dracoshell --themes  Pick a color theme (saved to config)");
    println!("    dracoshell --help    Show this message");
    println!("    dracoshell --version Show version");
    println!();
    println!("PANE KEYBINDINGS (Ctrl+Alt + …):");
    println!("    H / V                Split right / below");
    println!("    ← ↑ → ↓               Move focus between panes");
    println!("    W                    Close focused pane");
    println!("    Q                    Quit dracoshell");
    println!();
    println!("TAB KEYBINDINGS (Ctrl+Shift + …):");
    println!("    T                    New tab");
    println!("    1 .. 9               Switch to tab N");
    println!("    Tab                  Cycle to next tab");
    println!();
    println!("SCROLLBACK:");
    println!("    Mouse wheel          Scroll history of focused pane");
    println!("    Shift + PageUp/Down  Page through scrollback");
}

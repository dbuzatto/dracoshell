use std::sync::Arc;

use alacritty_terminal::event::{Event as TermEvent, EventListener, Notify, WindowSize};
use alacritty_terminal::event_loop::{EventLoop as PtyEventLoop, Msg, Notifier};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{Config as TermConfig, Term};
use alacritty_terminal::tty::{self, Options as PtyOptions, Shell};
use anyhow::{Context, Result};
use winit::event_loop::EventLoopProxy;

use crate::layout::PaneId;
use crate::UserEvent;

/// Bridges alacritty's terminal events back into winit's event loop so the
/// renderer can wake up and redraw when the PTY produces new output. Each
/// proxy is tagged with the pane id of the terminal that owns it.
#[derive(Clone)]
pub struct EventProxy {
    proxy: EventLoopProxy<UserEvent>,
    pane: PaneId,
}

impl EventListener for EventProxy {
    fn send_event(&self, event: TermEvent) {
        let _ = self.proxy.send_event(UserEvent::Term {
            pane: self.pane,
            event,
        });
    }
}

struct TermDims {
    cols: usize,
    lines: usize,
}

impl Dimensions for TermDims {
    fn total_lines(&self) -> usize {
        self.lines
    }
    fn screen_lines(&self) -> usize {
        self.lines
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

pub struct Terminal {
    pub term: Arc<FairMutex<Term<EventProxy>>>,
    notifier: Notifier,
    pub size: WindowSize,
}

impl Terminal {
    pub fn new(
        proxy: EventLoopProxy<UserEvent>,
        pane: PaneId,
        size: WindowSize,
        shell: Option<Shell>,
    ) -> Result<Self> {
        let event_proxy = EventProxy { proxy, pane };

        let pty_options = PtyOptions {
            shell,
            ..PtyOptions::default()
        };
        let pty = tty::new(&pty_options, size, pane).context("spawn pty")?;

        let dims = TermDims {
            cols: size.num_cols as usize,
            lines: size.num_lines as usize,
        };
        let term = Term::new(TermConfig::default(), &dims, event_proxy.clone());
        let term = Arc::new(FairMutex::new(term));

        let pty_loop = PtyEventLoop::new(term.clone(), event_proxy, pty, false, false)
            .context("spawn pty event loop")?;
        let notifier = Notifier(pty_loop.channel());
        pty_loop.spawn();

        Ok(Self {
            term,
            notifier,
            size,
        })
    }

    pub fn send_bytes(&self, bytes: Vec<u8>) {
        self.notifier.notify(bytes);
    }

    pub fn resize_to(&mut self, size: WindowSize) {
        if size.num_cols == self.size.num_cols && size.num_lines == self.size.num_lines {
            return;
        }
        self.size = size;
        let _ = self.notifier.0.send(Msg::Resize(size));
        let dims = TermDims {
            cols: size.num_cols as usize,
            lines: size.num_lines as usize,
        };
        let mut term = self.term.lock();
        term.resize(dims);
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = self.notifier.0.send(Msg::Shutdown);
    }
}

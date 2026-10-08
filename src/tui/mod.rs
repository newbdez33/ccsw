//! Full-screen dashboard (ratatui + crossterm), a port of cswap's TUI
//! (research notes `docs/research/cswap-tui.md`, spec §12).
//!
//! `app` is the pure state machine the render tests drive; `worker` owns the
//! threads; this file is the thin terminal loop around both.

pub mod app;
pub mod auto;
pub mod dashboard;
pub mod data;
pub mod modals;
pub mod snapshot;
pub mod switch;
pub mod theme;
pub mod watch;
pub mod widgets;
pub mod worker;

#[cfg(test)]
pub(crate) mod test_support;

use std::io::{self, IsTerminal};
use std::time::Duration;

use crossterm::event::{self, Event, KeyEventKind};

use crate::cli::tui::TuiStart;
use crate::paths::Paths;
use crate::store::Settings;

use app::App;
use theme::ThemeName;
use worker::{Runtime, now_s};

/// Leaves raw mode and the alternate screen however the loop ends.
struct RestoreGuard;

impl Drop for RestoreGuard {
    fn drop(&mut self) {
        ratatui::restore();
    }
}

/// Exit status: 0 on a clean quit, 1 without a terminal or when the terminal
/// could not be set up. TUI errors surface as toasts and modals, never as
/// exit codes.
pub fn run(start: TuiStart) -> i32 {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        eprintln!("Error: ccsw tui needs an interactive terminal (stdin and stdout)");
        return 1;
    }
    let paths = match Paths::from_env() {
        Ok(paths) => paths,
        Err(err) => {
            eprintln!("Error: {err}");
            return 1;
        }
    };
    let settings = Settings::load(&paths);
    let mut app = App::new(
        start,
        ThemeName::parse(&settings.theme),
        settings.autoswitch.threshold,
        std::env::var("COLORFGBG").ok(),
    );
    let mut runtime = Runtime::new(paths);
    let mut terminal = match ratatui::try_init() {
        Ok(terminal) => terminal,
        Err(err) => {
            eprintln!("Error: could not set up the terminal: {err}");
            return 1;
        }
    };
    let _guard = RestoreGuard;
    if let Err(err) = event_loop(&mut terminal, &mut app, &mut runtime) {
        drop(_guard);
        eprintln!("Error: {err}");
        return 1;
    }
    0
}

fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    runtime: &mut Runtime,
) -> io::Result<()> {
    loop {
        let now = now_s();
        app.tick(now);
        runtime.maybe_tick(app.store_only(), now);
        for message in runtime.drain() {
            let commands = app.receive(message, now);
            runtime.execute(commands, app, now);
        }
        app.set_refreshing_since(runtime.normal_started_at());
        terminal.draw(|frame| app.render(frame, now))?;
        if event::poll(Duration::from_millis(250))? {
            loop {
                match event::read()? {
                    Event::Key(key) if key.kind != KeyEventKind::Release => {
                        let commands = app.handle_key(key, now_s());
                        runtime.execute(commands, app, now_s());
                    }
                    _ => {}
                }
                if !event::poll(Duration::ZERO)? {
                    break;
                }
            }
        }
        if app.quit_requested() {
            return Ok(());
        }
    }
}

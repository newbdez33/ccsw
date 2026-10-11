//! The runtime side of the TUI: refresh lanes, mutating actions and the
//! hosted auto-switch engine, each on its own thread with a `Switcher` built
//! inside that thread (research notes `cswap-tui.md` §2.2–§2.3, §7).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::sync::{Arc, Mutex};
use std::thread;

use crate::autoswitch::{Engine, Event};
use crate::collect::CollectMode;
use crate::errors::Result;
use crate::model::SwitchOutcome;
use crate::paths::Paths;
use crate::provider::Provider;
use crate::store::settings::config_set;
use crate::store::{AutoSwitchSettings, Settings};
use crate::switcher::{Line as UiLine, SilentUi, Strategy, Switcher, Ui};

use super::app::{Action, ActionResult, App, Command, Inbound, Lane, POLL_INTERVAL_S};
use super::{console, console_host};

/// Records what the switcher says and answers every prompt with a yes: the
/// TUI has already confirmed through its own modals.
struct CollectingUi(Arc<Mutex<Vec<UiLine>>>);

impl Ui for CollectingUi {
    fn say(&mut self, line: UiLine) {
        self.0.lock().expect("ui lines").push(line);
    }

    fn confirm(&mut self, _prompt: &str) -> bool {
        true
    }

    fn ask(&mut self, _prompt: &str) -> Option<String> {
        None
    }
}

enum Msg {
    Snapshot {
        lane: Lane,
        generation: u64,
        taken_at: f64,
        result: std::result::Result<Option<crate::switcher::ListSnapshot>, String>,
    },
    Action(ActionResult),
    LoginUrl(String),
    Engine {
        id: u64,
        event: Event,
    },
    EngineStopped {
        id: u64,
        error: String,
    },
    Console(console::Event),
}

struct EngineHandle {
    id: u64,
    stop: Arc<AtomicBool>,
}

impl Drop for EngineHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

struct LoginHandle {
    cancel: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Drop for LoginHandle {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

pub struct Runtime {
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    paths: Paths,
    next_generation: u64,
    normal: Option<(u64, f64)>,
    store_inflight: bool,
    last_tick: Option<f64>,
    engine: Option<EngineHandle>,
    engine_seq: u64,
    login: Option<LoginHandle>,
    console: Option<console_host::Handle>,
}

/// Unix seconds with sub-second precision.
pub fn now_s() -> f64 {
    chrono::Utc::now().timestamp_millis() as f64 / 1000.0
}

impl Runtime {
    pub fn new(paths: Paths) -> Self {
        let (tx, rx) = channel();
        Self {
            tx,
            rx,
            paths,
            next_generation: 0,
            normal: None,
            store_inflight: false,
            last_tick: None,
            engine: None,
            engine_seq: 0,
            login: None,
            console: None,
        }
    }

    /// When the normal lane started, for the `refreshing 5s` status.
    pub fn normal_started_at(&self) -> Option<f64> {
        self.normal.map(|(_, at)| at)
    }

    /// Every 3 s: one lane per tick, store-only while the auto view is open,
    /// the store lane whenever the normal lane is busy.
    pub fn maybe_tick(&mut self, store_only: bool, now: f64) {
        let due = self
            .last_tick
            .is_none_or(|last| now - last >= POLL_INTERVAL_S);
        if due {
            self.tick(store_only, now);
        }
    }

    fn tick(&mut self, store_only: bool, now: f64) {
        self.last_tick = Some(now);
        if store_only {
            self.start_lane(Lane::Store, now);
        } else if self.normal.is_none() {
            self.start_lane(Lane::Normal, now);
        } else {
            self.start_lane(Lane::Store, now);
        }
    }

    fn start_lane(&mut self, lane: Lane, now: f64) {
        match lane {
            Lane::Normal if self.normal.is_some() => return,
            Lane::Store if self.store_inflight => return,
            _ => {}
        }
        self.next_generation += 1;
        let generation = self.next_generation;
        match lane {
            Lane::Normal => self.normal = Some((generation, now)),
            Lane::Store => self.store_inflight = true,
        }
        let tx = self.tx.clone();
        let mode = match lane {
            Lane::Normal => CollectMode::OnDemand,
            Lane::Store => CollectMode::StoreOnly,
        };
        thread::spawn(move || {
            let result = Switcher::from_env()
                .and_then(|switcher| switcher.list_snapshot(mode))
                .map_err(|err| err.to_string());
            let _ = tx.send(Msg::Snapshot {
                lane,
                generation,
                taken_at: now_s(),
                result,
            });
        });
    }

    /// A "full" refresh is the same on-demand pass (the usage store decides
    /// what to fetch); it only forces a tick now.
    fn request_refresh(&mut self, store_only: bool, now: f64) {
        self.tick(store_only, now);
    }

    fn run_action(&mut self, action: Action) {
        let tx = self.tx.clone();
        let is_login = action == Action::AddNew;
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        let thread = thread::spawn(move || {
            let result = perform(action, &worker_cancel, |url| {
                let _ = tx.send(Msg::LoginUrl(url));
            });
            let _ = tx.send(Msg::Action(result));
        });
        if is_login {
            self.login = Some(LoginHandle {
                cancel,
                thread: Some(thread),
            });
        }
    }

    fn cancel_login(&self) {
        if let Some(login) = &self.login {
            login.cancel.store(true, Ordering::SeqCst);
        }
    }

    fn stop_engine(&mut self) {
        self.engine = None;
    }

    fn start_engine(&mut self, settings: AutoSwitchSettings, dry_run: bool) {
        self.stop_engine();
        self.engine_seq += 1;
        let id = self.engine_seq;
        let stop = Arc::new(AtomicBool::new(false));
        self.engine = Some(EngineHandle {
            id,
            stop: stop.clone(),
        });
        let tx = self.tx.clone();
        thread::spawn(move || {
            let mut switcher = match Switcher::from_env() {
                Ok(switcher) => switcher,
                Err(err) => {
                    let _ = tx.send(Msg::EngineStopped {
                        id,
                        error: err.to_string(),
                    });
                    return;
                }
            };
            switcher.ui = Box::new(SilentUi);
            let sink = tx.clone();
            let mut engine = Engine::new(
                &mut switcher,
                Provider::Claude,
                settings,
                dry_run,
                move |event| {
                    let _ = sink.send(Msg::Engine {
                        id,
                        event: event.clone(),
                    });
                },
            );
            engine.run_loop(stop);
        });
    }

    /// Carry out what the app asked for.
    pub fn execute(&mut self, commands: Vec<Command>, app: &mut App, now: f64) {
        for command in commands {
            match command {
                Command::Quit => {
                    self.cancel_login();
                    if let Some(console) = &self.console {
                        console.stop();
                    }
                }
                Command::CancelLogin => self.cancel_login(),
                Command::Refresh { .. } => self.request_refresh(app.store_only(), now),
                Command::Action(action) => self.run_action(action),
                Command::OpenAuto => {
                    let settings = Settings::load(&self.paths).autoswitch;
                    let more = app.open_auto(settings, now);
                    self.execute(more, app, now);
                }
                Command::StartEngine { settings, dry_run } => self.start_engine(settings, dry_run),
                Command::StopEngine => self.stop_engine(),
                Command::Console(request) => match request {
                    console::Request::Start {
                        options,
                        open_browser,
                    } if self.console.is_none() => {
                        let tx = self.tx.clone();
                        self.console = Some(console_host::Handle::start(
                            self.paths.clone(),
                            options,
                            open_browser,
                            move |event| {
                                let _ = tx.send(Msg::Console(event));
                            },
                        ));
                    }
                    console::Request::NewLink { open_browser } => {
                        if let Some(console) = &self.console {
                            console.new_link(open_browser);
                        }
                    }
                    console::Request::Stop => {
                        if let Some(console) = &self.console {
                            console.stop();
                        }
                    }
                    _ => {}
                },
                Command::PersistTheme(name) => {
                    if let Err(err) = config_set(&self.paths, "ui.theme", name.as_str()) {
                        app.theme_save_failed(&err.to_string(), now);
                    }
                }
            }
        }
    }

    /// Everything the workers sent since the last call, with lane bookkeeping
    /// applied and events from a stopped engine dropped.
    pub fn drain(&mut self) -> Vec<Inbound> {
        let mut inbound = Vec::new();
        loop {
            match self.rx.try_recv() {
                Ok(Msg::Snapshot {
                    lane,
                    generation,
                    taken_at,
                    result,
                }) => {
                    match lane {
                        Lane::Normal => self.normal = None,
                        Lane::Store => self.store_inflight = false,
                    }
                    inbound.push(Inbound::Snapshot {
                        lane,
                        generation,
                        taken_at,
                        result,
                    });
                }
                Ok(Msg::Action(result)) => {
                    if result.action == Action::AddNew {
                        self.login = None;
                    }
                    inbound.push(Inbound::ActionDone(result));
                }
                Ok(Msg::LoginUrl(url)) => inbound.push(Inbound::LoginUrl(url)),
                Ok(Msg::Console(event)) => {
                    if matches!(event, console::Event::Stopped(_)) {
                        self.console = None;
                    }
                    inbound.push(Inbound::Console(event));
                }
                Ok(Msg::Engine { id, event }) => {
                    if self.engine.as_ref().is_some_and(|e| e.id == id) {
                        inbound.push(Inbound::Engine(event));
                    }
                }
                Ok(Msg::EngineStopped { id, error }) => {
                    if self.engine.as_ref().is_some_and(|e| e.id == id) {
                        self.engine = None;
                        inbound.push(Inbound::EngineStopped(error));
                    }
                }
                Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
            }
        }
        inbound
    }
}

/// Run one action against a fresh switcher whose prompts auto-confirm.
fn perform(action: Action, cancel: &AtomicBool, on_url: impl FnMut(String)) -> ActionResult {
    let lines = Arc::new(Mutex::new(Vec::new()));
    let outcome: Result<Option<(SwitchOutcome, Option<String>)>> = (|| {
        let mut switcher = Switcher::from_env()?;
        switcher.ui = Box::new(CollectingUi(lines.clone()));
        match &action {
            Action::AddNew => {
                match crate::codex::login::browser_login(&switcher.store.paths, cancel, on_url)? {
                    Some(auth) => {
                        switcher.add_browser_account(&auth)?;
                    }
                    None => switcher.ui.say(UiLine::plain("Login cancelled.")),
                }
                Ok(None)
            }
            Action::SwitchTo(number) => Ok(switcher
                .switch_to(&number.to_string(), false, false)?
                .map(|report| (report.outcome, report.followup))),
            Action::SwitchBest(provider) => {
                let models = switcher.settings.autoswitch.model_names();
                let report = switcher.switch(Some(*provider), Strategy::Best, &models, false)?;
                Ok(Some((report.outcome, report.followup)))
            }
            Action::SetDisabled { number, disabled } => {
                switcher.set_disabled(&number.to_string(), *disabled)?;
                Ok(None)
            }
            Action::Remove(number) => {
                switcher.remove(&number.to_string(), false)?;
                Ok(None)
            }
            Action::AddCurrent => {
                switcher.add_accounts(None, None, None)?;
                Ok(None)
            }
            Action::AddToken(form) => {
                switcher.add_token(&form.token, form.email.as_deref(), form.slot.map(i64::from))?;
                Ok(None)
            }
        }
    })();
    let mut lines = lines.lock().expect("ui lines").clone();
    match outcome {
        Ok(switch) => {
            let (switch, followup) = switch.unzip();
            ActionResult {
                action,
                ok: true,
                lines,
                switch,
                followup: followup.flatten(),
            }
        }
        Err(err) => {
            lines.push(UiLine::warning(format!("Error: {err}")));
            ActionResult {
                action,
                ok: false,
                lines,
                switch: None,
                followup: None,
            }
        }
    }
}

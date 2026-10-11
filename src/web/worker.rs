use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::collect::CollectMode;
use crate::model::now_unix;
use crate::paths::Paths;
use crate::store::{Settings, Store};
use crate::switcher::{SilentUi, Switcher};

use super::Shared;
use super::api::{Effects, Failure, Snapshot, SwitchRequest};
use super::auth::Session;

pub(super) enum Command {
    Refresh,
    Switch {
        session: Session,
        request: SwitchRequest,
    },
    Stop,
}

#[derive(Clone, Serialize)]
pub(super) struct EventRecord {
    title: String,
    detail: String,
    time: i64,
}

fn record(app: &Shared, title: &str, detail: impl Into<String>) {
    let mut activity = app.activity.lock().expect("activity lock");
    activity.insert(
        0,
        EventRecord {
            title: title.into(),
            detail: detail.into(),
            time: now_unix(),
        },
    );
    activity.truncate(50);
}

fn publish(app: &Shared, switcher: &Switcher, external: bool) {
    match Snapshot::capture(switcher) {
        Ok(snapshot) => {
            let mut previous = app.snapshot.write().expect("snapshot lock");
            if external
                && previous
                    .as_ref()
                    .is_some_and(|old| old.active != snapshot.active)
            {
                record(
                    app,
                    "Active account changed on the host",
                    "Observed from the local login state.",
                );
            }
            *previous = Some(snapshot);
        }
        Err(_) => {
            if let Some(snapshot) = app.snapshot.write().expect("snapshot lock").as_mut() {
                snapshot.stale = true;
            }
        }
    }
    let _ = app.changes.send(());
}

fn refresh(app: &Shared, switcher: &mut Switcher, manual: bool) {
    let _ = app.changes.send(());
    switcher.settings = Settings::load(&switcher.store.paths);
    let result = switcher.list_snapshot(CollectMode::OnDemand);
    app.refreshing.store(false, Ordering::Relaxed);
    if manual {
        record(
            app,
            if result.is_ok() {
                "Usage refresh completed"
            } else {
                "Usage refresh failed"
            },
            "The shared cache and provider poll budgets apply.",
        );
    }
    publish(app, switcher, true);
}

fn switch(app: &Shared, switcher: &mut Switcher, session: Session, request: SwitchRequest) {
    let result = if !app.auth.lock().expect("auth lock").valid(&session) {
        Err(crate::errors::CcswError::Conflict("unauthorized"))
    } else if app.config.read_only {
        Err(crate::errors::CcswError::Conflict("read_only"))
    } else {
        switcher.switch_guarded(
            request.slot,
            request.provider,
            &request.expected_revision,
            request.acknowledge_interruption,
        )
    };
    let before = app
        .snapshot
        .read()
        .expect("snapshot lock")
        .as_ref()
        .and_then(|s| s.active.get(request.provider));
    publish(app, switcher, false);
    let after = app
        .snapshot
        .read()
        .expect("snapshot lock")
        .as_ref()
        .and_then(|s| s.active.get(request.provider));
    let mut operations = app.operations.lock().expect("operation lock");
    if let Some(stored) = operations.get_mut(&(session.id, request.request_id)) {
        match result {
            Ok(report) => {
                let effects = Effects::from_effect(report.effect.as_ref());
                stored.operation.state = if effects.daemon == "failed" {
                    "partial"
                } else {
                    "succeeded"
                };
                stored.operation.effects = Some(effects);
            }
            Err(error) => {
                let rejected = matches!(
                    error,
                    crate::errors::CcswError::Conflict(_)
                        | crate::errors::CcswError::Lock(_)
                        | crate::errors::CcswError::Session(_)
                );
                let partial = !rejected && before != after && after == Some(request.slot);
                stored.operation.state = if partial { "partial" } else { "failed" };
                stored.operation.error = Some(if partial {
                    Failure::new("switch_partial")
                } else {
                    Failure::core(&error)
                });
            }
        }
        let followup = stored
            .operation
            .error
            .as_ref()
            .map(|error| error.message)
            .or_else(|| {
                stored
                    .operation
                    .effects
                    .as_ref()
                    .map(|effects| effects.followup)
            })
            .unwrap_or_default();
        record(
            app,
            "Account switch",
            format!(
                "{} · #{} · {}. {}",
                request.provider, request.slot, stored.operation.state, followup
            ),
        );
    }
    drop(operations);
    let _ = app.changes.send(());
}

pub(super) fn run(app: Shared, paths: Paths, receiver: Receiver<Command>) {
    let mut switcher = Switcher::open(Store::open(paths));
    switcher.ui = Box::new(SilentUi);
    publish(&app, &switcher, false);
    let mut last_refresh = Instant::now();
    let mut last_snapshot = Instant::now();
    while !app.stopped.load(Ordering::Relaxed) {
        match receiver.recv_timeout(Duration::from_millis(250)) {
            Ok(Command::Stop) | Err(RecvTimeoutError::Disconnected) => break,
            Ok(Command::Switch { session, request }) => {
                switch(&app, &mut switcher, session, request)
            }
            Ok(Command::Refresh) => {
                refresh(&app, &mut switcher, true);
                last_refresh = Instant::now();
                last_snapshot = Instant::now();
            }
            Err(RecvTimeoutError::Timeout) => {}
        }
        if last_snapshot.elapsed() >= Duration::from_secs(3) {
            publish(&app, &switcher, true);
            last_snapshot = Instant::now();
        }
        if last_refresh.elapsed() >= Duration::from_secs(30)
            && !app.refreshing.swap(true, Ordering::Relaxed)
        {
            refresh(&app, &mut switcher, false);
            last_refresh = Instant::now();
            last_snapshot = Instant::now();
        }
    }
}

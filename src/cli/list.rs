//! Human rendering of accounts and usage: `list`, `status`, and the account
//! list printed after a switch.

use std::io::{self, BufRead, Write};

use crate::codex::auth::AuthJson;
use crate::codex::jwt::token_expires_at;
use crate::errors::Result;
use crate::jsonout;
use crate::model::{CurrentAccount, now_unix};
use crate::printer::{countdown_and_clock, format_age};
use crate::store::credentials;
use crate::store::poll_policy::SERVE_TTL_S;
use crate::store::usage_store::{UsageEntry, UsageSentinel};
use crate::switcher::{AccountRow, Line, ListSnapshot, StatusSnapshot, Style, Switcher};
use crate::usage_math::{binding_pct, usage_rows};

/// `  <n>: <label> [<tag>] (active) (disabled)`.
pub fn account_line(row: &AccountRow) -> Line {
    let record = &row.record;
    let mut line = Line::plain(format!("  {}: ", row.slot));
    match record.alias.as_deref().filter(|a| !a.is_empty()) {
        Some(alias) => {
            line = line
                .push(Style::Accent, alias)
                .push(Style::Plain, format!(" ({})", record.email));
        }
        None => line = line.push(Style::Plain, record.email.clone()),
    }
    line = line
        .push(Style::Plain, " ")
        .push(Style::Muted, format!("[{}]", record.display_tag()));
    if row.is_active {
        line = line.push(Style::BoldAccent, " (active)");
    }
    if record.disabled {
        line = line.push(Style::Muted, " (disabled)");
    }
    line
}

/// The tree-connected usage lines under an account (`indent` precedes each).
pub fn usage_lines(entry: &UsageEntry, now: i64, indent: &str) -> Vec<Line> {
    let mut lines = Vec::new();
    if let Some(sentinel) = entry.sentinel {
        lines.push(Line::plain(indent).push(Style::Dimmed, sentinel.label()));
        if sentinel != UsageSentinel::ApiKey
            && let Some(last) = &entry.last_good
            && let Some(pct) = binding_pct(last, &[])
        {
            let age = entry
                .age_s
                .map_or_else(|| "unknown age".to_string(), format_age);
            lines.push(
                Line::plain(indent)
                    .push(Style::Dimmed, "└ ")
                    .push(Style::Muted, format!("last seen {pct:.0}% used · {age}")),
            );
        }
        return lines;
    }
    let Some(usage) = &entry.last_good else {
        let mut text = "usage unavailable".to_string();
        if let Some(error) = &entry.last_error {
            text.push_str(&format!(" ({error})"));
        }
        lines.push(Line::plain(indent).push(Style::Dimmed, text));
        return lines;
    };
    let rows = usage_rows(usage, entry.fetched_at);
    let width = rows.iter().map(|r| r.label.len() + 1).max().unwrap_or(0);
    let mut texts: Vec<String> = rows
        .iter()
        .map(|row| {
            let label = format!("{}:", row.label);
            let mut body = format!("{:>3.0}%", row.pct);
            if let Some(reset) = row.resets_at {
                let (countdown, clock) = countdown_and_clock(reset, now);
                body.push_str(&format!("   resets {clock:<12}  in {countdown}"));
            }
            let scoped = row.label != "5h" && row.label != "7d";
            if scoped && row.maxed {
                body.push_str("  (!)");
            }
            format!("{label:<width$} {body}")
        })
        .collect();
    if let Some(credits) = &usage.credits {
        match (credits.balance, credits.unlimited) {
            (Some(balance), _) => texts.push(format!("credits: ${balance:.2}")),
            (None, true) => texts.push("credits: unlimited".to_string()),
            (None, false) => {}
        }
    }
    if texts.is_empty() {
        lines.push(Line::plain(indent).push(Style::Dimmed, "usage unavailable"));
        return lines;
    }
    if let Some(age) = entry.age_s.filter(|age| *age > SERVE_TTL_S)
        && let Some(last) = texts.last_mut()
    {
        last.push_str(&format!(" · {}", format_age(age)));
    }
    let count = texts.len();
    for (i, text) in texts.into_iter().enumerate() {
        let connector = if i + 1 == count { "└ " } else { "├ " };
        lines.push(
            Line::plain(indent)
                .push(Style::Dimmed, connector)
                .push(Style::Muted, text),
        );
    }
    lines
}

/// `fresh|expired, refresh token yes|no, expires <clock> in <countdown>` or
/// `unknown expiry, refresh token yes|no`.
fn token_status(auth: &AuthJson, now: i64) -> String {
    let refresh = if auth.refresh_token().is_some() {
        "yes"
    } else {
        "no"
    };
    match auth.access_token().and_then(token_expires_at) {
        Some(exp) => {
            let state = if exp <= now { "expired" } else { "fresh" };
            let (countdown, clock) = countdown_and_clock(exp, now);
            format!("{state}, refresh token {refresh}, expires {clock} in {countdown}")
        }
        None => format!("unknown expiry, refresh token {refresh}"),
    }
}

/// The `--token-status` bullet lines for one account.
fn token_status_lines(switcher: &Switcher, row: &AccountRow, now: i64) -> Vec<Line> {
    if row.record.is_api_key() {
        return Vec::new();
    }
    let bullet = |text: String| Line::plain("     ").push(Style::Muted, format!("• {text}"));
    let mut lines = Vec::new();
    if row.is_active
        && let Ok(Some(live)) = AuthJson::read(&switcher.store.paths.live_auth_file())
    {
        lines.push(bullet(format!(
            "active profile: {}",
            token_status(&live, now)
        )));
    }
    let stored = credentials::read(&switcher.store, row.slot)
        .ok()
        .flatten()
        .map(AuthJson::from_value);
    let status = match stored {
        Some(auth) => token_status(&auth, now),
        None => "no stored credentials".to_string(),
    };
    lines.push(bullet(format!("stored backup: {status}")));
    lines
}

/// The whole `Accounts:` block.
pub fn list_lines(switcher: &Switcher, snapshot: &ListSnapshot, token_status: bool) -> Vec<Line> {
    let now = now_unix();
    let mut lines = vec![Line::new().push(Style::Bold, "Accounts:")];
    let count = snapshot.rows.len();
    for (i, row) in snapshot.rows.iter().enumerate() {
        lines.push(account_line(row));
        lines.extend(usage_lines(&row.usage, now, "     "));
        if token_status {
            lines.extend(token_status_lines(switcher, row, now));
        }
        if i + 1 < count {
            lines.push(Line::new());
        }
    }
    if !snapshot.warnings.is_empty() {
        lines.push(Line::new());
        for warning in &snapshot.warnings {
            lines.push(Line::warning(warning));
        }
    }
    lines
}

pub fn status_lines(snapshot: &StatusSnapshot) -> Vec<Line> {
    let header = Line::new().push(Style::Bold, "Status:");
    match (&snapshot.current, &snapshot.row) {
        (CurrentAccount::NoLogin, _) => {
            vec![header.push(Style::Dimmed, " No active Codex account")]
        }
        (CurrentAccount::Managed { slot, .. }, Some(row)) => {
            let mut lines = vec![
                header
                    .push(Style::Plain, " ")
                    .push(Style::Accent, format!("Account-{slot}"))
                    .push(Style::Plain, format!(" ({} ", row.record.email))
                    .push(Style::Muted, format!("[{}]", row.record.display_tag()))
                    .push(Style::Plain, ")"),
                Line::plain("  ").push(
                    Style::Dimmed,
                    format!("Total managed accounts: {}", snapshot.total),
                ),
            ];
            lines.extend(usage_lines(&row.usage, now_unix(), "  "));
            lines
        }
        (current, _) => {
            let email = current
                .email()
                .filter(|e| !e.is_empty())
                .unwrap_or("API key");
            vec![header.push(Style::Dimmed, format!(" {email} (not managed)"))]
        }
    }
}

pub fn print_lines(lines: &[Line]) {
    for line in lines {
        println!("{}", line.render());
    }
}

/// `list` / `ls`.
pub fn list_cmd(switcher: &mut Switcher, json: bool, token_status: bool) -> Result<i32> {
    let Some(snapshot) = switcher.list_snapshot(true)? else {
        return first_run(switcher, json);
    };
    if json {
        let now = now_unix();
        let rows: Vec<serde_json::Value> = snapshot
            .rows
            .iter()
            .map(|row| jsonout::account_row(row.slot, &row.record, &row.usage, row.is_active, now))
            .collect();
        print!(
            "{}",
            jsonout::render_document(&jsonout::list_payload(
                snapshot.active,
                rows,
                &snapshot.warnings
            ))
        );
        return Ok(0);
    }
    print_lines(&list_lines(switcher, &snapshot, token_status));
    Ok(0)
}

/// `list` before any roster exists: offer to add the live login (human mode).
fn first_run(switcher: &mut Switcher, json: bool) -> Result<i32> {
    if json {
        print!(
            "{}",
            jsonout::render_document(&jsonout::list_payload(None, Vec::new(), &[]))
        );
        return Ok(0);
    }
    print_lines(&[Line::dimmed("No accounts are managed yet.")]);
    let current = switcher.current_account()?;
    let (CurrentAccount::Unmanaged { email } | CurrentAccount::Managed { email, .. }) = current
    else {
        print_lines(&[Line::dimmed(
            "No active Codex account found. Please log in first.",
        )]);
        return Ok(0);
    };
    let shown = if email.is_empty() {
        "API key"
    } else {
        email.as_str()
    };
    print!("No managed accounts found. Add current account ({shown}) to managed list? [Y/n] ");
    let _ = io::stdout().flush();
    let mut answer = String::new();
    let declined = match io::stdin().lock().read_line(&mut answer) {
        Ok(0) | Err(_) => true,
        Ok(_) => matches!(answer.trim().to_lowercase().as_str(), "n" | "no"),
    };
    if declined {
        print_lines(&[Line::dimmed(
            "Setup cancelled. You can run 'cswitch add' later.",
        )]);
        return Ok(0);
    }
    switcher.add_account(None, None)?;
    Ok(0)
}

pub fn status_cmd(switcher: &mut Switcher, json: bool) -> Result<i32> {
    let snapshot = switcher.status()?;
    if json {
        let managed = snapshot.row.as_ref().map(|row| (&row.record, &row.usage));
        print!(
            "{}",
            jsonout::render_document(&jsonout::status_payload(
                &snapshot.current,
                managed,
                snapshot.total,
                now_unix()
            ))
        );
        return Ok(0);
    }
    print_lines(&status_lines(&snapshot));
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        AccountRecord, Credits, NormalizedUsage, ScopedWindow, WindowUsage, format_iso,
    };

    fn entry() -> UsageEntry {
        UsageEntry {
            sentinel: None,
            last_good: None,
            fetched_at: None,
            age_s: None,
            last_attempt_at: None,
            consecutive_failures: 0,
            last_error: None,
            backoff_until: None,
            next_poll_at: None,
            poll_interval_s: None,
            last_429_at: None,
            auth_dead_strikes: 0,
            struck_fingerprint: None,
            trust_extended: false,
        }
    }

    fn texts(lines: &[Line]) -> Vec<String> {
        lines.iter().map(Line::text).collect()
    }

    #[test]
    fn account_line_markers() {
        let mut record = AccountRecord::new("a@x.io");
        let row = AccountRow {
            slot: 1,
            record: record.clone(),
            usage: entry(),
            is_active: true,
        };
        assert_eq!(account_line(&row).text(), "  1: a@x.io [personal] (active)");
        record.alias = Some("dev".into());
        record.organization_name = "Acme".into();
        record.disabled = true;
        let row = AccountRow {
            slot: 12,
            record,
            usage: entry(),
            is_active: false,
        };
        assert_eq!(
            account_line(&row).text(),
            "  12: dev (a@x.io) [Acme] (disabled)"
        );
    }

    #[test]
    fn usage_lines_measurement_sentinel_and_unavailable() {
        let now = 1_790_000_000;
        let mut e = entry();
        e.last_good = Some(NormalizedUsage {
            five_hour: Some(WindowUsage {
                pct: 10.0,
                resets_at: Some(format_iso(now + 5400)),
            }),
            seven_day: Some(WindowUsage {
                pct: 50.4,
                resets_at: None,
            }),
            scoped: vec![ScopedWindow {
                name: "GPT-5.3-Codex-Spark".into(),
                pct: 100.0,
                resets_at: None,
            }],
            credits: Some(Credits {
                balance: Some(12.5),
                unlimited: false,
            }),
            limited: false,
            plan_type: None,
        });
        e.fetched_at = Some(now as f64 - 10.0);
        e.age_s = Some(10.0);
        let lines = texts(&usage_lines(&e, now, "     "));
        assert_eq!(lines.len(), 4);
        assert!(
            lines[0].starts_with("     ├ 5h:                   10%   resets "),
            "{}",
            lines[0]
        );
        assert!(lines[0].ends_with("  in 1h 30m"), "{}", lines[0]);
        assert_eq!(lines[1], "     ├ 7d:                   50%");
        assert_eq!(lines[2], "     ├ GPT-5.3-Codex-Spark: 100%  (!)");
        assert_eq!(lines[3], "     └ credits: $12.50");

        e.age_s = Some(400.0);
        let lines = texts(&usage_lines(&e, now, "  "));
        assert_eq!(lines[3], "  └ credits: $12.50 · 6m ago");

        e.sentinel = Some(UsageSentinel::TokenExpired);
        let lines = texts(&usage_lines(&e, now, "  "));
        assert_eq!(
            lines,
            [
                "  token expired — refresh deferred this pass; retries automatically",
                "  └ last seen 50% used · 6m ago",
            ]
        );
        e.sentinel = Some(UsageSentinel::ApiKey);
        assert_eq!(texts(&usage_lines(&e, now, "")), ["API key (no quota)"]);

        let mut e = entry();
        assert_eq!(texts(&usage_lines(&e, now, "  ")), ["  usage unavailable"]);
        e.last_error = Some("http-429".into());
        assert_eq!(
            texts(&usage_lines(&e, now, "  ")),
            ["  usage unavailable (http-429)"]
        );
        e.sentinel = Some(UsageSentinel::NoCredentials);
        assert_eq!(texts(&usage_lines(&e, now, "")), ["no credentials"]);
    }

    #[test]
    fn status_lines_variants() {
        let no_login = StatusSnapshot {
            current: CurrentAccount::NoLogin,
            total: 0,
            row: None,
        };
        assert_eq!(
            texts(&status_lines(&no_login)),
            ["Status: No active Codex account"]
        );
        let unmanaged = StatusSnapshot {
            current: CurrentAccount::Unmanaged {
                email: "u@x.io".into(),
            },
            total: 1,
            row: None,
        };
        assert_eq!(
            texts(&status_lines(&unmanaged)),
            ["Status: u@x.io (not managed)"]
        );
        let managed = StatusSnapshot {
            current: CurrentAccount::Managed {
                slot: 1,
                email: "a@x.io".into(),
                api_key: false,
            },
            total: 2,
            row: Some(AccountRow {
                slot: 1,
                record: AccountRecord::new("a@x.io"),
                usage: entry(),
                is_active: true,
            }),
        };
        assert_eq!(
            texts(&status_lines(&managed)),
            [
                "Status: Account-1 (a@x.io [personal])",
                "  Total managed accounts: 2",
                "  usage unavailable",
            ]
        );
    }
}

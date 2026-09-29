//! `switch`, `switch <id>`, `switch --strategy`.

use crate::errors::Result;
use crate::jsonout;
use crate::store::parse_model_names;
use crate::switcher::{Line, Strategy, SwitchReport, Switcher};

use super::list::{list_lines, print_lines};

/// `switch <NUM|EMAIL|ALIAS> [--force]`.
pub fn direct_cmd(
    switcher: &mut Switcher,
    identifier: &str,
    force: bool,
    json: bool,
) -> Result<i32> {
    let Some(report) = switcher.switch_to(identifier, force, !json)? else {
        return Ok(0);
    };
    finish(switcher, report, json, None)
}

/// Bare `switch` and `switch --strategy … [--model …]`.
pub fn rotate_cmd(
    switcher: &mut Switcher,
    strategy: Option<&str>,
    model: Option<&str>,
    json: bool,
) -> Result<i32> {
    let strategy = Strategy::from_flag(strategy)?;
    let (models, source) = match (strategy, model) {
        (Strategy::Rotation, _) => (Vec::new(), ""),
        (_, Some(names)) => (parse_model_names(names), "cli"),
        (_, None) => (
            switcher.settings.autoswitch.model_names(),
            "autoswitch.model",
        ),
    };
    if !json && !models.is_empty() {
        let origin = if source == "cli" {
            "--model"
        } else {
            "autoswitch.model"
        };
        print_lines(&[Line::dimmed(format!(
            "Using configured model limits: {} (from {origin})",
            models.join(", ")
        ))]);
    }
    let report = switcher.switch(strategy, &models, !json)?;
    finish(switcher, report, json, Some((models.as_slice(), source)))
}

fn finish(
    switcher: &mut Switcher,
    report: SwitchReport,
    json: bool,
    models: Option<(&[String], &str)>,
) -> Result<i32> {
    if json {
        print!(
            "{}",
            jsonout::render_document(&jsonout::switch_payload(&report.outcome, models))
        );
        return Ok(0);
    }
    if report.show_list {
        match switcher.list_snapshot(true) {
            Ok(Some(snapshot)) => print_lines(&list_lines(switcher, &snapshot, false)),
            _ => print_lines(&[Line::plain(
                "  (usage display unavailable — run cswitch list to retry)",
            )]),
        }
    }
    if let Some(followup) = report.followup {
        println!();
        print_lines(&[Line::dimmed(followup)]);
        println!();
    }
    Ok(0)
}

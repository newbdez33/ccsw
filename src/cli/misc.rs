//! The pre-dispatched `alias`, `swap`, `move` verbs, plus `purge`, `menubar`
//! and `upgrade`.

use crate::errors::Result;
use crate::switcher::Switcher;

use super::{VerbArgs, usage_error_for, with_switcher};

const ALIAS_USAGE: &str = "usage: ccsw alias [-h] [--unset] [--debug] [NUM|EMAIL] [NAME]";
const SWAP_USAGE: &str = "usage: ccsw swap [-h] [--debug] NUM|EMAIL NUM|EMAIL";
const MOVE_USAGE: &str = "usage: ccsw move [-h] [--debug] NUM|EMAIL SLOT";

pub fn alias_cmd(argv: Vec<String>) -> i32 {
    let args = match VerbArgs::parse(&argv, &["--unset"]) {
        Ok(args) => args,
        Err(msg) => return usage_error_for("alias", ALIAS_USAGE, &msg),
    };
    if args.help {
        println!(
            "{ALIAS_USAGE}

Set, remove, or list a short display alias for an account. Once set, the alias
can be used anywhere an account number or email is accepted (switch, remove,
run, map).

positional arguments:
  NUM|EMAIL   account to alias
  NAME        the alias to set

options:
  -h, --help  show this help message and exit
  --unset     remove the account's alias
  --debug     Enable debug logging

examples:
  ccsw alias 2 dev
  ccsw alias user@example.com dev
  ccsw alias 2 --unset
  ccsw alias                         # list all aliases"
        );
        return 0;
    }
    let unset = args.has("--unset");
    let error = |msg: &str| usage_error_for("alias", ALIAS_USAGE, msg);
    match (args.positionals.as_slice(), unset) {
        ([], false) => with_switcher(args.debug, false, |sw| {
            sw.alias_list()?;
            Ok(0)
        }),
        ([_, _], true) => error("--unset does not take a NAME argument"),
        ([], true) => error("NUM|EMAIL is required with --unset"),
        ([id], true) => {
            let id = id.clone();
            with_switcher(args.debug, false, |sw| {
                sw.alias_unset(&id)?;
                Ok(0)
            })
        }
        ([_], false) => error("NAME is required (or pass --unset to remove the alias)"),
        ([id, name], false) => {
            let (id, name) = (id.clone(), name.clone());
            with_switcher(args.debug, false, |sw| {
                sw.alias_set(&id, &name)?;
                Ok(0)
            })
        }
        (extra, _) => error(&format!("unrecognized arguments: {}", extra[2..].join(" "))),
    }
}

pub fn swap_cmd(argv: Vec<String>) -> i32 {
    let args = match VerbArgs::parse(&argv, &[]) {
        Ok(args) => args,
        Err(msg) => return usage_error_for("swap", SWAP_USAGE, &msg),
    };
    if args.help {
        println!(
            "{SWAP_USAGE}

Exchange two accounts' slot numbers, so they trade places in `ccsw list` and
as numeric targets. Aliases, backups, and session history move with their
account.

examples:
  ccsw swap 1 2
  ccsw swap dev user@example.com"
        );
        return 0;
    }
    match args.positionals.as_slice() {
        [a, b] => {
            let (a, b) = (a.clone(), b.clone());
            with_switcher(args.debug, false, |sw| {
                sw.swap_accounts(&a, &b)?;
                Ok(0)
            })
        }
        [_] => usage_error_for(
            "swap",
            SWAP_USAGE,
            "the following arguments are required: NUM|EMAIL",
        ),
        [] => usage_error_for(
            "swap",
            SWAP_USAGE,
            "the following arguments are required: NUM|EMAIL, NUM|EMAIL",
        ),
        extra => usage_error_for(
            "swap",
            SWAP_USAGE,
            &format!("unrecognized arguments: {}", extra[2..].join(" ")),
        ),
    }
}

pub fn move_cmd(argv: Vec<String>) -> i32 {
    let args = match VerbArgs::parse(&argv, &[]) {
        Ok(args) => args,
        Err(msg) => return usage_error_for("move", MOVE_USAGE, &msg),
    };
    if args.help {
        println!(
            "{MOVE_USAGE}

Assign an account to a slot number. An empty slot relocates the account there
and frees its old slot; an occupied slot swaps the two. Aliases, backups, and
session history move with the account.

examples:
  ccsw move user@example.com 1   move an account onto shortcut 1
  ccsw move dev 1                by alias
  ccsw move 2 1                  by number (swaps if slot 1 is taken)"
        );
        return 0;
    }
    match args.positionals.as_slice() {
        [id, slot] => {
            let (id, slot) = (id.clone(), slot.clone());
            with_switcher(args.debug, false, |sw| {
                sw.move_account(&id, &slot)?;
                Ok(0)
            })
        }
        [_] => usage_error_for(
            "move",
            MOVE_USAGE,
            "the following arguments are required: SLOT",
        ),
        [] => usage_error_for(
            "move",
            MOVE_USAGE,
            "the following arguments are required: NUM|EMAIL, SLOT",
        ),
        extra => usage_error_for(
            "move",
            MOVE_USAGE,
            &format!("unrecognized arguments: {}", extra[2..].join(" ")),
        ),
    }
}

pub fn purge(switcher: &mut Switcher) -> Result<i32> {
    switcher.purge()?;
    Ok(0)
}

pub fn menubar() -> i32 {
    eprintln!("The menu bar is not available in ccsw.");
    1
}

/// Guidance only: ccsw does not upgrade itself.
pub fn upgrade() -> i32 {
    eprintln!(
        "ccsw does not upgrade itself. To install the latest release, run:
  cargo install --git https://github.com/newbdez33/ccsw --locked
or download a release binary from:
  https://github.com/newbdez33/ccsw/releases"
    );
    1
}

//! `add`, `add-token`, `remove`, `disable`, `enable`.

use std::io::{self, BufRead, IsTerminal};

use crate::errors::{CswitchError, Result};
use crate::provider::Provider;
use crate::switcher::Switcher;

pub fn add(
    switcher: &mut Switcher,
    provider: Option<Provider>,
    slot: Option<i64>,
    alias: Option<&str>,
) -> Result<i32> {
    switcher.add_accounts(provider, slot, alias)?;
    Ok(0)
}

/// `TOKEN` literal `-` reads one stdin line; an empty value prompts without echo.
pub fn add_token(
    switcher: &mut Switcher,
    token: &str,
    email: Option<&str>,
    slot: Option<i64>,
) -> Result<i32> {
    let token = match token {
        "-" => read_stdin_line()?,
        "" => prompt_token()?,
        given => given.to_string(),
    };
    switcher.add_token(&token, email, slot)?;
    Ok(0)
}

fn read_stdin_line() -> Result<String> {
    let mut line = String::new();
    io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|err| CswitchError::validation(format!("could not read the token: {err}")))?;
    Ok(line.trim_end_matches(['\n', '\r']).to_string())
}

fn prompt_token() -> Result<String> {
    if io::stdin().is_terminal() {
        rpassword::prompt_password("Token: ")
            .map_err(|err| CswitchError::validation(format!("could not read the token: {err}")))
    } else {
        read_stdin_line()
    }
}

pub fn remove(switcher: &mut Switcher, identifier: &str) -> Result<i32> {
    switcher.remove(identifier, true)?;
    Ok(0)
}

pub fn set_disabled(switcher: &mut Switcher, identifier: &str, disabled: bool) -> Result<i32> {
    switcher.set_disabled(identifier, disabled)?;
    Ok(0)
}

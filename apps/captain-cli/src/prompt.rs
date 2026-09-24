//! Terminal prompts for login and `auth init`. Prompts go to stderr so stdout stays JSON.

use std::io::{BufRead, Write};

use captain_protocol::LoginPrompt;
use captain_vault::Secret;

/// Asks on the controlling terminal.
pub(crate) struct Terminal;

impl LoginPrompt for Terminal {
    fn phone(&mut self) -> std::io::Result<String> {
        line("Phone number (international, e.g. +84...): ")
    }

    fn code(&mut self) -> std::io::Result<String> {
        line("Login code from Telegram: ")
    }

    fn password(&mut self, hint: Option<&str>) -> std::io::Result<Secret> {
        let prompt = match hint {
            Some(hint) if !hint.is_empty() => format!("Two-step password (hint: {hint}): "),
            _ => "Two-step password: ".to_owned(),
        };
        hidden(&prompt)
    }
}

/// Reads one visible line from stdin after printing `prompt` to stderr.
pub(crate) fn line(prompt: &str) -> std::io::Result<String> {
    let mut err = std::io::stderr().lock();
    err.write_all(prompt.as_bytes())?;
    err.flush()?;
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer)?;
    Ok(answer.trim().to_owned())
}

/// Reads one line without echoing it.
pub(crate) fn hidden(prompt: &str) -> std::io::Result<Secret> {
    rpassword::prompt_password(prompt).map(Secret::new)
}

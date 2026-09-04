// Roughcut — a keyboard-driven assembly editor that exports MLT XML.
// Copyright (C) 2026 Roughcut contributors
//
// This program is free software: you can redistribute it and/or modify it
// under the terms of the GNU General Public License as published by the Free
// Software Foundation, either version 3 of the License, or (at your option)
// any later version. It is distributed WITHOUT ANY WARRANTY; see the GNU
// General Public License at <https://www.gnu.org/licenses/> for details.

//! Roughcut without the window: one command table, reached from a shell or
//! from an MCP client.
//!
//! The window is still the good way to *watch* footage, which is most of
//! editing. This is for the other half — importing a folder, finding the
//! sentence somebody said, assembling from what a transcript turned up,
//! exporting — none of which needs a picture on screen, and all of which is
//! tedious by hand.

mod args;
mod mcp;
mod ops;
mod spec;

use serde_json::Value;

fn main() -> std::process::ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    match dispatch(&argv) {
        Ok(Some(value)) => {
            println!("{}", serde_json::to_string_pretty(&value).expect("output is JSON"));
            std::process::ExitCode::SUCCESS
        }
        Ok(None) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            // Errors are JSON too, on standard error. Anything reading this
            // programmatically gets one shape from both streams, and anything
            // reading it as a person still gets a sentence.
            let text = format!("{e:#}");
            eprintln!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({ "error": text }))
                    .unwrap_or(text)
            );
            std::process::ExitCode::FAILURE
        }
    }
}

/// `Ok(None)` when something was printed that is not a command result — the
/// usage text, or the MCP server having run to completion.
fn dispatch(argv: &[String]) -> anyhow::Result<Option<Value>> {
    let Some(name) = argv.first().map(String::as_str) else {
        print!("{}", spec::usage());
        return Ok(None);
    };

    match name {
        "help" | "--help" | "-h" => {
            match argv.get(1).and_then(|n| spec::find(n)) {
                Some(cmd) => print!("{}", cmd.usage()),
                None => print!("{}", spec::usage()),
            }
            return Ok(None);
        }
        "--version" | "-V" => {
            println!("roughcut-cli {}", env!("CARGO_PKG_VERSION"));
            return Ok(None);
        }
        "mcp" => {
            mcp::serve()?;
            return Ok(None);
        }
        _ => {}
    }

    let cmd = spec::find(name).ok_or_else(|| {
        let close: Vec<&str> = spec::COMMANDS
            .iter()
            .map(|c| c.name)
            .filter(|c| c.contains(name) || name.contains(c))
            .collect();
        match close.as_slice() {
            [] => anyhow::anyhow!("there is no command called {name}. Run `roughcut-cli` for the list."),
            near => anyhow::anyhow!("there is no command called {name}. Did you mean {}?", near.join(" or ")),
        }
    })?;

    let parsed = args::parse_argv(cmd, &argv[1..])?;
    ops::run(cmd, &parsed).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn no_arguments_prints_the_usage_rather_than_failing() {
        assert!(dispatch(&[]).unwrap().is_none());
        assert!(dispatch(&argv(&["help"])).unwrap().is_none());
        assert!(dispatch(&argv(&["help", "append"])).unwrap().is_none());
    }

    #[test]
    fn a_near_miss_suggests_the_real_command() {
        let e = dispatch(&argv(&["clip"])).unwrap_err().to_string();
        assert!(e.contains("clips"), "{e}");
        let e = dispatch(&argv(&["frobnicate"])).unwrap_err().to_string();
        assert!(e.contains("no command"), "{e}");
    }
}

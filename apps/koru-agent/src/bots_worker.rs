// SPDX-License-Identifier: MPL-2.0

//! The Bots engine role is dispatched before Agent logging and singleton work.

use std::path::PathBuf;

pub fn role() -> koru_agent_worker::Role {
    koru_agent_worker::Role { name: "bots", run }
}

pub fn bot_role() -> koru_agent_worker::Role {
    koru_agent_worker::Role {
        name: "bot",
        run: run_bot,
    }
}

pub fn parse_bot(args: &[String]) -> Result<uuid::Uuid, &'static str> {
    let [flag, id] = args else {
        return Err("invalid_request");
    };
    if flag != "--bot" {
        return Err("invalid_request");
    }
    uuid::Uuid::parse_str(id)
        .ok()
        .filter(|id| !id.is_nil())
        .ok_or("invalid_request")
}

fn run_bot(args: Vec<String>) -> ! {
    let bot = match parse_bot(&args) {
        Ok(bot) => bot,
        Err(code) => {
            eprintln!("{code}\nusage: koru-agent --worker bot --bot <id>");
            std::process::exit(2)
        }
    };
    match koru_bots_engine::bot_worker::run_supervised(bot) {
        Ok(()) => std::process::exit(0),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1)
        }
    }
}

pub fn parse(args: impl IntoIterator<Item = String>) -> Result<PathBuf, &'static str> {
    let mut args = args.into_iter();
    if args.next().as_deref() != Some("--root") {
        return Err("invalid_request");
    }
    let root = args
        .next()
        .filter(|word| !word.starts_with('-'))
        .ok_or("invalid_request")?;
    if args.next().is_some() || !PathBuf::from(&root).is_absolute() {
        return Err("invalid_request");
    }
    Ok(root.into())
}

fn run(args: Vec<String>) -> ! {
    let root = match parse(args) {
        Ok(root) => root,
        Err(code) => {
            eprintln!("{code}\nusage: koru-agent --worker bots --root <absolute-path>");
            std::process::exit(2)
        }
    };
    match koru_bots_engine::worker::run_supervised(&root) {
        Ok(()) => std::process::exit(0),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1)
        }
    }
}

#[cfg(test)]
mod tests;

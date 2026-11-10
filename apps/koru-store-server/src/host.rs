// SPDX-License-Identifier: MPL-2.0

//! Standalone process adapter for the reusable Store worker.

use std::path::PathBuf;

use koru_channel as channel;
use koru_store_core::errors::{Code, Error, Result};

use crate::{
    StoreWorker, WorkerConfig, cli, console, log, projection, root_inspection, search,
    store_backup, tasks,
};

pub fn run() {
    let invocation = match cli::parse(std::env::args()) {
        Ok(i) => i,
        Err(rejected) => rejected.report(),
    };

    if invocation.command != cli::Command::Serve || invocation.console {
        console::attach();
    }

    let result = match &invocation.command {
        cli::Command::Help => {
            print!("{}", cli::help());
            return;
        }
        cli::Command::Version => {
            println!("{}", cli::version());
            return;
        }
        cli::Command::Serve => root_for(&invocation).and_then(|root| {
            let ready = std::env::var_os(crate::INTERNAL_READY_ENV)
                .map(PathBuf::from)
                .unwrap_or_else(|| root.join("standalone-ready.json"));
            StoreWorker::new(WorkerConfig::standalone(
                root,
                channel::segment(),
                invocation.console,
                Some(ready),
            ))
            .serve()
        }),
        cli::Command::Projection(target) => root_for(&invocation)
            .and_then(|root| projection::report_to_console(&root, target.as_deref())),
        cli::Command::Search(text) => {
            root_for(&invocation).and_then(|root| search::report_to_console(&root, text.as_deref()))
        }
        cli::Command::Tasks(filter) => root_for(&invocation)
            .and_then(|root| tasks::report_to_console(&root, filter.as_deref())),
        cli::Command::Backup(destination) => root_for(&invocation)
            .and_then(|root| store_backup::backup_from_cli(&root, destination.as_deref())),
        cli::Command::Restore(source) => {
            root_for(&invocation).and_then(|root| store_backup::restore_from_cli(&root, source))
        }
        cli::Command::RootCatalog => {
            root_for(&invocation).and_then(|root| root_inspection::report_to_console(&root))
        }
    };

    if let Err(error) = result {
        if invocation.command == cli::Command::Serve && !invocation.console {
            log::report_fatal(&format!("The Koru Store could not start.\n\n{error}"));
            std::process::exit(1);
        }
        eprintln!("koru-store-server: {error}");
        std::process::exit(1);
    }
}

fn root_for(invocation: &cli::Invocation) -> Result<PathBuf> {
    match &invocation.root {
        Some(explicit) => Ok(explicit.clone()),
        None => channel::store_root().ok_or_else(|| {
            Error::new(
                Code::StorageFailed,
                "could not work out where this machine keeps local application data",
            )
        }),
    }
}

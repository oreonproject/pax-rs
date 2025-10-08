use metadata::get_local_deps;
use tokio::runtime::Runtime;
use utils::{choice, is_root};

use crate::{Command, PostAction, StateBox};

pub fn build_remove(hierarchy: &[String]) -> Command {
    Command::new(
        "remove",
        vec![String::from("r")],
        "Removes a package, whilst maintaining any user-made configurations",
        vec![utils::specific_flag(), utils::yes_flag()],
        None,
        remove,
        hierarchy,
    )
}

pub fn build_purge(hierarchy: &[String]) -> Command {
    Command::new(
        "purge",
        vec![String::from("p")],
        "Removes a package, WITHOUT maintaining any user-made configurations",
        vec![utils::specific_flag(), utils::yes_flag()],
        None,
        purge,
        hierarchy,
    )
}

fn remove(states: &StateBox, args: Option<&[String]>) -> PostAction {
    run(states, args, false)
}

fn purge(states: &StateBox, args: Option<&[String]>) -> PostAction {
    run(states, args, true)
}

fn run(states: &StateBox, args: Option<&[String]>, purge: bool) -> PostAction {
    if !is_root() {
        return PostAction::Elevate;
    }
    let mut args = match args {
        None => return PostAction::NothingToDo,
        Some(args) => args.iter(),
    };
    let mut data = Vec::new();
    if states.get("specific").is_some_and(|x| *x) {
        while let Some(name) = args.next()
            && let Some(ver) = args.next()
        {
            data.push((name, Some(ver)));
        }
    } else {
        args.for_each(|x| data.push((x, None)));
    }
    let runtime = match Runtime::new() {
        Ok(runtime) => runtime,
        Err(_) => {
            println!("Error creating runtime!");
            return PostAction::Return;
        }
    };
    match runtime.block_on(get_local_deps(&data)) {
        Ok(metadatas) => {
            println!();
            if metadatas.is_empty() {
                return PostAction::NothingToDo;
            }
            let msg = if purge { "PURGED: " } else { "REMOVED:" };
            println!(
                "\nThe following package(s) will be {msg}  \x1B[91m{}\x1B[0m",
                metadatas
                    .remove
                    .iter()
                    .fold(String::new(), |acc, x| format!("{acc} {}", x.name))
                    .trim()
            );
            if metadatas.has_deps() {
                println!(
                    "The following package(s) will be MODIFIED: \x1B[93m{}\x1B[0m",
                    metadatas
                        .modify
                        .iter()
                        .fold(String::new(), |acc, x| format!("{acc} {}", x.name))
                        .trim()
                );
                if states.get("yes").is_none_or(|x: &bool| !*x) {
                    match choice("Continue?", true) {
                        Err(message) => {
                            println!("{message}");
                            return PostAction::Return;
                        }
                        Ok(false) => {
                            println!("Aborted.");
                            return PostAction::Return;
                        }
                        Ok(true) => (),
                    };
                }
            }
            for package in metadatas.remove {
                match package.remove_version(purge) {
                    Ok(()) => (),
                    Err(message) => {
                        println!("Operation failed!\nReported Error: \"\x1B[91m{message}\x1B[0m\"");
                        println!("\x1B[91m=== YOU MAY HAVE BROKEN PACKAGES! ===\x1B[0m");
                        return PostAction::Return;
                    }
                };
            }
        }
        Err(fault) => {
            println!("\x1B[2K\r\x1B[91m{fault}\x1B[0m");
        }
    };
    PostAction::Return
}

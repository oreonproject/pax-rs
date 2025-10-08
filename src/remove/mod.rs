use flags::Flag;
use metadata::get_local_deps;
use tokio::runtime::Runtime;
use utils::{choice, is_root};

use crate::{Command, PostAction, StateBox};

pub fn build(hierarchy: &[String]) -> Command {
    let specific = Flag::new(
        Some('s'),
        "specific",
        "Makes every second argument the target version for the argument prior.",
        false,
        false,
        |states, _| {
            states.shove("specific", true);
        },
    );
    Command::new(
        "remove",
        vec![String::from("r")],
        "Removes a package, whilst maintaining any user-made configurations",
        vec![specific, utils::yes_flag()],
        None,
        run,
        hierarchy,
    )
}

fn run(states: &StateBox, args: Option<&[String]>) -> PostAction {
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
            println!(
                "\nThe following packages will be REMOVED:  \x1B[91m{}\x1B[0m",
                metadatas
                    .remove
                    .iter()
                    .fold(String::new(), |acc, x| format!("{acc} {}", x.name))
                    .trim()
            );
            if metadatas.has_deps() {
                println!(
                    "The following packages will be MODIFIED: \x1B[93m{}\x1B[0m",
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
            //
        }
        Err(fault) => {
            println!("\x1B[2K\r{fault}");
        }
    };
    PostAction::Return
}

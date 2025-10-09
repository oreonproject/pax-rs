use metadata::collect_upgrades;
use settings::acquire_lock;
use tokio::runtime::Runtime;

use crate::{Command, PostAction, StateBox};

pub fn build(hierarchy: &[String]) -> Command {
    Command::new(
        "update",
        vec![String::from("d")],
        "Downloads the upgrade metadata for non-phased packages.",
        vec![utils::yes_flag()],
        None,
        run,
        hierarchy,
    )
}

fn run(_states: &StateBox, _args: Option<&[String]>) -> PostAction {
    match acquire_lock() {
        Ok(Some(action)) => return action,
        Err(fault) => {
            println!("\x1B[91m{fault}\x1B[0m");
            return PostAction::Return;
        }
        _ => (),
    }
    let runtime = match Runtime::new() {
        Ok(runtime) => runtime,
        Err(_) => {
            println!("Error creating runtime!");
            return PostAction::Return;
        }
    };
    match runtime.block_on(collect_upgrades()) {
        Ok(()) => (),
        Err(fault) => {
            println!("\x1B[2K\r\x1B[91m{fault}\x1B[0m");
        }
    }
    PostAction::Return
}

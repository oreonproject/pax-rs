use metadata::collect_upgrades;
use tokio::runtime::Runtime;
use utils::is_root;

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
    if !is_root() {
        return PostAction::Elevate;
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

use metadata::collect_updates;
use settings::acquire_lock;
use tokio::runtime::Runtime;

use crate::{Command, PostAction, StateBox};

pub fn build(hierarchy: &[String]) -> Command {
    Command::new(
        "update",
        vec![String::from("d")],
        "Downloads the upgrade metadata for non-phased packages.",
        Vec::new(),
        None,
        run,
        hierarchy,
    )
}

fn run(_states: &StateBox, _args: Option<&[String]>) -> PostAction {
    match acquire_lock() {
        Ok(Some(action)) => return action,
        Err(fault) => return PostAction::Fuck(fault),
        _ => (),
    }
    let Ok(runtime) = Runtime::new() else {
        return PostAction::Fuck(String::from("Error creating runtime!"));
    };
    if let Err(fault) = runtime.block_on(collect_updates()) {
        PostAction::Fuck(fault)
    } else {
        PostAction::Return
    }
}

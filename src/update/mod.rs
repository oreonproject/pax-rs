use commands::Command;
use metadata::collect_updates;
use settings::acquire_lock;
use statebox::StateBox;
use tokio::runtime::Runtime;
use utils::{PostAction, choice};

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

fn run(states: &StateBox, _args: Option<&[String]>) -> PostAction {
    match acquire_lock() {
        Ok(Some(action)) => return action,
        Err(fault) => return PostAction::Fuck(fault),
        _ => (),
    }
    
    let Ok(runtime) = Runtime::new() else {
        return PostAction::Fuck(String::from("Error creating runtime!"));
    };
    
    let updates = match runtime.block_on(collect_updates()) {
        Ok(updates) => updates,
        Err(fault) => return PostAction::Fuck(fault),
    };
    
    if updates.is_empty() {
        println!("No updates available.");
        return PostAction::Return;
    }
    
    // Show available updates
    println!(
        "The following package(s) will be UPDATED: \x1B[94m{}\x1B[0m",
        updates.iter()
            .fold(String::new(), |acc, x| format!("{acc} {}", x.name))
            .trim()
    );
    
    // Add confirmation prompt unless --yes flag is used
    if states.get("yes").is_none_or(|x: &bool| !*x) {
        match choice("Continue with updates?", true) {
            Err(message) => return PostAction::Fuck(message),
            Ok(false) => return PostAction::Fuck(String::from("Aborted.")),
            Ok(true) => (),
        };
    }
    
    PostAction::Return
}

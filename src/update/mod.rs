use commands::Command;
use metadata::{collect_updates, upgrade_packages};
use settings::acquire_lock;
use statebox::StateBox;
use tokio::runtime::Runtime;
use utils::{PostAction, choice};

pub fn build(hierarchy: &[String]) -> Command {
    Command::new(
        "update",
        vec![String::from("d")],
        "Check for updates and upgrade packages. Shows summary with y/n prompt, or use --yes/-y to skip.",
        vec![utils::yes_flag(), utils::refresh_flag()],
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

    // Collect available updates
    let refresh_cache = states.get("refresh_cache").is_some_and(|x: &bool| *x);
    let updates = match runtime.block_on(collect_updates(refresh_cache)) {
        Ok(updates) => updates,
        Err(fault) => return PostAction::Fuck(fault),
    };

    if updates.is_empty() {
        println!("No updates available.");
        return PostAction::Return;
    }

    // Show available updates summary
    println!("\x1B[92mPackage Updates Available\x1B[0m");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!();

    for update in &updates {
        println!("  \x1B[94m{}\x1B[0m -> \x1B[92m{}\x1B[0m", update.name, update.version);
        if !update.description.is_empty() {
            println!("    {}", update.description);
        }
        println!();
    }

    println!("Total: {} package(s) to upgrade", updates.len());

    // Add confirmation prompt unless --yes flag is used
    if states.get("yes").is_none_or(|x: &bool| !*x) {
        match choice("Continue with updates?", true) {
            Err(message) => return PostAction::Fuck(message),
            Ok(false) => return PostAction::Fuck(String::from("Aborted.")),
            Ok(true) => (),
        };
    }

    // Perform the upgrades
    println!("\x1B[92mUpgrading packages...\x1B[0m");

    let refresh_cache = states.get("refresh_cache").is_some_and(|x: &bool| *x);
    let package_names: Vec<String> = updates.iter().map(|u| u.name.clone()).collect();
    match runtime.block_on(upgrade_packages(package_names, refresh_cache)) {
        Ok(_) => {
            println!("\x1B[92mAll packages upgraded successfully!\x1B[0m");
            PostAction::Return
        }
        Err(fault) => PostAction::Fuck(fault),
    }
}

use commands::Command;
use metadata::{upgrade_all, upgrade_only, upgrade_packages};
use settings::acquire_lock;
use statebox::StateBox;
use utils::{PostAction, choice};
use tokio::runtime::Runtime;

pub fn build(hierarchy: &[String]) -> Command {
    Command::new(
        "upgrade",
        vec![String::from("g")],
        "Upgrades a non-phased package from its upgrade metadata.",
        vec![utils::yes_flag(), utils::refresh_flag()],
        None,
        run,
        hierarchy,
    )
}

fn run(states: &StateBox, args: Option<&[String]>) -> PostAction {
    match acquire_lock() {
        Ok(Some(action)) => return action,
        Err(fault) => return PostAction::Fuck(fault),

        _ => (),
    }
    let args = if let Some(args) = args {
        let mut args = args.iter();
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
        data
    } else {
        Vec::new()
    };
    let Ok(runtime) = Runtime::new() else {
        return PostAction::Fuck(String::from("Error creating runtime!"));
    };
    let refresh_cache = states.get("refresh_cache").is_some_and(|x: &bool| *x);
    let data = match if args.is_empty() {
        runtime.block_on(upgrade_all(refresh_cache))
    } else {
        let package_names: Vec<String> = args.iter().map(|(name, _)| (*name).clone()).collect();
        runtime.block_on(upgrade_only(package_names, refresh_cache))
    } {
        Ok(data) => data,
        Err(fault) => return PostAction::Fuck(fault),
    };
    if data.is_empty() {
        return PostAction::NothingToDo;
    }
    println!(
        "The following package(s) will be UPGRADED: \x1B[94m{}\x1B[0m",
        data.join(" ")
    );
    if states.get("yes").is_none_or(|x: &bool| !*x) {
        match choice("Continue?", true) {
            Err(message) => return PostAction::Fuck(message),
            Ok(false) => return PostAction::Fuck(String::from("Aborted.")),
            Ok(true) => (),
        };
    }
    if let Err(fault) = runtime.block_on(upgrade_packages(data, refresh_cache)) {
        return PostAction::Fuck(fault);
    }
    PostAction::Return
}

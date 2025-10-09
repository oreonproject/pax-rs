use commands::Command;
use metadata::{upgrade_all, upgrade_only};
use settings::acquire_lock;
use statebox::StateBox;
use utils::PostAction;

pub fn build(hierarchy: &[String]) -> Command {
    Command::new(
        "upgrade",
        vec![String::from("g")],
        "Upgrades a non-phased package from its upgrade metadata.",
        vec![utils::yes_flag()],
        None,
        run,
        hierarchy,
    )
}

fn run(states: &StateBox, args: Option<&[String]>) -> PostAction {
    match acquire_lock() {
        Ok(Some(action)) => return action,
        Err(fault) => {
            println!("\x1B[91m{fault}\x1B[0m");
            return PostAction::Return;
        }
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
    let data = match if args.is_empty() {
        upgrade_all()
    } else {
        upgrade_only(&args)
    } {
        Ok(data) => data,
        Err(fault) => {
            println!("\x1B[2K\r\x1B[91m{fault}\x1B[0m");
            return PostAction::Return;
        }
    };
    PostAction::Return
}

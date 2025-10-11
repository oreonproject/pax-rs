use metadata::emancipate;
use settings::acquire_lock;

use crate::{Command, PostAction, StateBox};

pub fn build(hierarchy: &[String]) -> Command {
    Command::new(
        // WHat the fuck? Yes, I will hopefully find a better name...
        "emancipate",
        vec![String::from("e")],
        "Marks a dependent package as independent.",
        vec![utils::specific_flag()],
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
    if let Err(fault) = emancipate(&data) {
        PostAction::Fuck(fault)
    } else {
        PostAction::Return
    }
}

use commands::Command;
use flags::Flag;
use metadata::list_installed_packages;
use settings::check_root_required;
use statebox::StateBox;
use utils::{PostAction};

pub fn build(hierarchy: &[String]) -> Command {
    let show_deps = Flag::new(
        Some('d'),
        "deps",
        "Show dependencies for each package",
        false,
        false,
        |states, _| {
            states.shove("show_deps", true);
        },
    );
    
    let show_dependents = Flag::new(
        Some('r'),
        "reverse",
        "Show dependents (packages that depend on this one)",
        false,
        false,
        |states, _| {
            states.shove("show_dependents", true);
        },
    );
    
    let filter = Flag::new(
        Some('f'),
        "filter",
        "Filter packages by name pattern",
        true,
        false,
        |states, arg| {
            if let Some(pattern) = arg {
                states.shove("filter_pattern", pattern.clone());
            }
        },
    );

    Command::new(
        "list",
        vec![String::from("l")],
        "List all installed packages",
        vec![show_deps, show_dependents, filter],
        None,
        run,
        hierarchy,
    )
}

fn run(states: &StateBox, _args: Option<&[String]>) -> PostAction {
    // List is read-only, doesn't require root
    if let Some(action) = check_root_required(false) {
        return action;
    }

    let show_deps = states.get::<bool>("show_deps").is_some_and(|x| *x);
    let show_dependents = states.get::<bool>("show_dependents").is_some_and(|x| *x);
    let filter_pattern = states.get::<String>("filter_pattern").map(|x| x.clone());

    match list_installed_packages(show_deps, show_dependents, filter_pattern.as_deref()) {
        Ok(packages) => {
            if packages.is_empty() {
                println!("\x1B[95mNo packages installed\x1B[0m");
            } else {
                let filter_msg = if let Some(pattern) = &filter_pattern {
                    format!(" (filtered by '{}')", pattern)
                } else {
                    String::new()
                };
                
                println!("\x1B[92mInstalled packages{}:\x1B[0m", filter_msg);
                println!();
                
                for (i, package) in packages.iter().enumerate() {
                    println!("\x1B[94m{}. {}\x1B[0m", i + 1, package.name);
                    println!("   \x1B[90mVersion:\x1B[0m {}", package.version);
                    println!("   \x1B[90mOrigin:\x1B[0m {}", package.origin);
                    
                    if package.dependent {
                        println!("   \x1B[93m[DEPENDENT]\x1B[0m");
                    } else {
                        println!("   \x1B[92m[INDEPENDENT]\x1B[0m");
                    }
                    
                    if show_deps && !package.dependencies.is_empty() {
                        let dep_names: Vec<String> = package.dependencies.iter().map(|dep| dep.name.clone()).collect();
                        println!("   \x1B[90mDependencies:\x1B[0m {}", dep_names.join(", "));
                    }
                    
                    if show_dependents && !package.dependents.is_empty() {
                        let dep_names: Vec<String> = package.dependents.iter().map(|dep| dep.name.clone()).collect();
                        println!("   \x1B[90mDependents:\x1B[0m {}", dep_names.join(", "));
                    }
                    
                    println!();
                }
                
                println!("\x1B[90mTotal: {} package(s)\x1B[0m", packages.len());
            }
            PostAction::Return
        }
        Err(fault) => PostAction::Fuck(fault),
    }
}

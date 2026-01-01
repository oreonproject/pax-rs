use commands::Command;
use flags::Flag;
use metadata::search_packages;
use settings::{check_root_required, SettingsYaml};
use statebox::StateBox;
use tokio::runtime::Runtime;
use utils::{PostAction};

pub fn build(hierarchy: &[String]) -> Command {
    let exact = Flag::new(
        Some('e'),
        "exact",
        "Only show packages that exactly match the search term",
        false,
        false,
        |states, _| {
            states.shove("exact", true);
        },
    );
    
    let installed = Flag::new(
        Some('i'),
        "installed",
        "Only search through installed packages",
        false,
        false,
        |states, _| {
            states.shove("installed", true);
        },
    );
    
    let show_deps = Flag::new(
        Some('d'),
        "deps",
        "Show dependencies for each package found",
        false,
        false,
        |states, _| {
            states.shove("show_deps", true);
        },
    );

    let remote = Flag::new(
        Some('r'),
        "remote",
        "Search remote repositories in addition to installed packages",
        false,
        false,
        |states, _| {
            states.shove("remote", true);
        },
    );

    Command::new(
        "search",
        vec![String::from("s")],
        "Search for packages by name or description",
        vec![exact, installed, show_deps, remote],
        None,
        run,
        hierarchy,
    )
}

fn run(states: &StateBox, args: Option<&[String]>) -> PostAction {
    // Search is read-only, doesn't require root
    if let Some(action) = check_root_required(false) {
        return action;
    }
    let args = match args {
        None => return PostAction::Fuck(String::from("No search term provided!")),
        Some(args) => args,
    };

    if args.is_empty() {
        return PostAction::Fuck(String::from("No search term provided!"));
    }

    let search_term = args.join(" ");
    let exact_match = states.get::<bool>("exact").is_some_and(|x| *x);
    let installed_only = states.get::<bool>("installed").is_some_and(|x| *x) ||
        !states.get::<bool>("remote").is_some_and(|x| *x); // Default to installed only unless --remote is specified
    let show_deps = states.get::<bool>("show_deps").is_some_and(|x| *x);

    // Get settings if we're not searching installed only
    let settings = if !installed_only {
        match SettingsYaml::get_settings() {
            Ok(settings) => Some(settings),
            Err(_) => return PostAction::PullSources,
        }
    } else {
        None
    };

    let Ok(runtime) = Runtime::new() else {
        return PostAction::Fuck(String::from("Error creating runtime!"));
    };

    match runtime.block_on(search_packages(
        &search_term,
        exact_match,
        installed_only,
        show_deps,
        settings.as_ref(),
    )) {
        Ok(results) => {
            if results.is_empty() {
                println!("\x1B[95mNo packages found matching '{}'\x1B[0m", search_term);
            } else {
                println!("\x1B[92mFound {} package(s) matching '{}':\x1B[0m", results.len(), search_term);
                println!();
                
                for (i, result) in results.iter().enumerate() {
                    println!("\x1B[94m{}. {}\x1B[0m", i + 1, result.name);
                    println!("   \x1B[90mVersion:\x1B[0m {}", result.version);
                    println!("   \x1B[90mDescription:\x1B[0m {}", result.description);
                    
                    if show_deps && !result.dependencies.is_empty() {
                        println!("   \x1B[90mDependencies:\x1B[0m {}", result.dependencies.join(", "));
                    }
                    
                    if result.installed {
                        println!("   \x1B[92m[INSTALLED]\x1B[0m");
                    }
                    
                    println!();
                }
            }
            PostAction::Return
        }
        Err(fault) => PostAction::Fuck(fault),
    }
}

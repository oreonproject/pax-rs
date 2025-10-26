use commands::Command;
use flags::Flag;
use metadata::get_package_info;
use settings::{acquire_lock, SettingsYaml};
use statebox::StateBox;
use tokio::runtime::Runtime;
use utils::{PostAction};

pub fn build(hierarchy: &[String]) -> Command {
    let show_files = Flag::new(
        Some('f'),
        "files",
        "Show files installed by this package",
        false,
        false,
        |states, _| {
            states.shove("show_files", true);
        },
    );
    
    let show_deps = Flag::new(
        Some('d'),
        "deps",
        "Show dependencies and dependents",
        false,
        false,
        |states, _| {
            states.shove("show_deps", true);
        },
    );
    
    let show_versions = Flag::new(
        Some('v'),
        "versions",
        "Show all available versions",
        false,
        false,
        |states, _| {
            states.shove("show_versions", true);
        },
    );

    Command::new(
        "info",
        vec![String::from("in")],
        "Show detailed information about a package",
        vec![show_files, show_deps, show_versions],
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

    let args = match args {
        None => return PostAction::Fuck(String::from("No package name provided!")),
        Some(args) => args,
    };

    if args.is_empty() {
        return PostAction::Fuck(String::from("No package name provided!"));
    }

    let package_name = &args[0];
    let show_files = states.get::<bool>("show_files").is_some_and(|x| *x);
    let show_deps = states.get::<bool>("show_deps").is_some_and(|x| *x);
    let show_versions = states.get::<bool>("show_versions").is_some_and(|x| *x);

    // Get settings for available package info
    let settings = match SettingsYaml::get_settings() {
        Ok(settings) => Some(settings),
        Err(_) => return PostAction::PullSources,
    };

    let Ok(runtime) = Runtime::new() else {
        return PostAction::Fuck(String::from("Error creating runtime!"));
    };

    match runtime.block_on(get_package_info(
        package_name,
        show_files,
        show_deps,
        show_versions,
        settings.as_ref(),
    )) {
        Ok(info) => {
            println!("\x1B[94mPackage Information: {}\x1B[0m", info.name);
            println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
            println!();
            
            println!("\x1B[90mDescription:\x1B[0m {}", info.description);
            println!("\x1B[90mVersion:\x1B[0m {}", info.version);
            println!("\x1B[90mOrigin:\x1B[0m {}", info.origin);
            println!("\x1B[90mPackage Type:\x1B[0m {}", info.package_type);
            
            if info.installed {
                println!("\x1B[92mStatus:\x1B[0m \x1B[92m[INSTALLED]\x1B[0m");
                if info.dependent {
                    println!("\x1B[93mDependency Status:\x1B[0m \x1B[93m[DEPENDENT]\x1B[0m");
                } else {
                    println!("\x1B[92mDependency Status:\x1B[0m \x1B[92m[INDEPENDENT]\x1B[0m");
                }
            } else {
                println!("\x1B[95mStatus:\x1B[0m \x1B[95m[NOT INSTALLED]\x1B[0m");
            }
            
            if show_deps {
                println!();
                println!("\x1B[90mDependencies:\x1B[0m");
                if info.dependencies.is_empty() {
                    println!("  None");
                } else {
                    for dep in &info.dependencies {
                        println!("  • {}", dep);
                    }
                }
                
                if !info.dependents.is_empty() {
                    println!();
                    println!("\x1B[90mDependents:\x1B[0m");
                    for dep in &info.dependents {
                        println!("  • {}", dep);
                    }
                }
            }
            
            if show_files && !info.installed_files.is_empty() {
                println!();
                println!("\x1B[90mInstalled Files:\x1B[0m");
                for file in &info.installed_files {
                    println!("  • {}", file);
                }
            }
            
            if show_versions && !info.available_versions.is_empty() {
                println!();
                println!("\x1B[90mAvailable Versions:\x1B[0m");
                for version in &info.available_versions {
                    println!("  • {}", version);
                }
            }
            
            println!();
            PostAction::Return
        }
        Err(fault) => PostAction::Fuck(fault),
    }
}

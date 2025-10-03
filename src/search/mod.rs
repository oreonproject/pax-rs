use crate::database::Database;
use crate::repository::create_client_from_settings;
use crate::{Command, PostAction, StateBox};
use settings::get_settings;

pub fn build(hierarchy: &[String]) -> Command {
    Command::new(
        "search",
        vec![String::from("s"), String::from("find")],
        "Search for packages in repositories",
        Vec::new(),
        None,
        run,
        hierarchy,
    )
}

fn run(_: &StateBox, args: Option<&[String]>) -> PostAction {
    let args = match args {
        None => {
            println!("Usage: pax search <pattern>");
            return PostAction::Return;
        }
        Some(args) => args,
    };

    if args.is_empty() {
        println!("Usage: pax search <pattern>");
        return PostAction::Return;
    }

    let pattern = &args[0];

    // load settings
    let settings = match get_settings() {
        Ok(s) => s,
        Err(_) => return PostAction::PullSources,
    };

    // initialize repository client
    let repo_client = match create_client_from_settings(&settings) {
        Ok(c) => c,
        Err(e) => {
            println!("Failed to create repository client: {}", e);
            return PostAction::Return;
        }
    };

    // open database to check installed status
    let db = Database::open("/opt/pax/db/pax.db").ok();

    println!("Searching for '{}'...\n", pattern);

    let results = repo_client.search_pattern(pattern);

    if results.is_empty() {
        println!("No packages found matching '{}'", pattern);
        return PostAction::Return;
    }

    let mut total_found = 0;
    for (source, packages) in results {
        println!("\x1B[36m{}:\x1B[0m", source);
        
        for pkg in packages {
            total_found += 1;
            
            // check if installed
            let installed = if let Some(ref db) = db {
                db.is_installed(&pkg.name).unwrap_or(false)
            } else {
                false
            };

            let status = if installed {
                "\x1B[32m[installed]\x1B[0m"
            } else {
                ""
            };

            println!("  \x1B[33m{}\x1B[0m {} {}", pkg.name, pkg.version, status);
            
            // truncate long descriptions
            let desc = if pkg.description.len() > 70 {
                format!("{}...", &pkg.description[..67])
            } else {
                pkg.description.clone()
            };
            
            println!("    {}", desc);
        }
        println!();
    }

    println!("Found {} package(s)", total_found);

    PostAction::Return
}


use crate::database::Database;
use crate::repository::{create_client_from_settings, RepositoryClient};
use crate::{Command, PostAction, StateBox};
use settings::{get_settings, get_settings_or_local};
use std::collections::HashMap;

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

    // Load settings - use local-only settings if endpoints.txt doesn't exist
    let settings = match get_settings_or_local() {
        Ok(s) => s,
        Err(_) => return PostAction::Return,
    };

    // Initialize repository client (if sources are configured)
    let repo_client = if settings.sources.is_empty() {
        println!("No repository sources configured. Search only available for installed packages.");
        None
    } else {
        match create_client_from_settings(&settings) {
            Ok(c) => {
                println!("Searching repositories for '{}'...", pattern);

                // Search in repositories first
                match c.search_package(pattern) {
                    Ok(Some((source, pkg_entry))) => {
                        println!("Found in repository:");
                        println!("  {} (version {}) from {}", pkg_entry.name, pkg_entry.version, source);
                        println!("  {}", pkg_entry.description);
                        println!();
                    }
                    Ok(None) => {
                        println!("No packages found in repositories matching '{}'", pattern);
                        println!();
                    }
                    Err(e) => {
                        println!("Error searching repositories: {}", e);
                        println!();
                    }
                }
                Some(c)
            }
            Err(e) => {
                println!("Failed to create repository client: {}", e);
                println!();
                None
            }
        }
    };

    // Open database to check installed status
    let db = Database::open("/opt/pax/db/pax.db").ok();

    println!("Searching for '{}'...\n", pattern);

    // Search using repo_client if available, otherwise just search locally
    let results = if let Some(ref client) = repo_client {
        client.search_pattern(pattern)
    } else {
        // No repositories configured, return empty results for repository search
        std::collections::HashMap::new()
    };

    // If no repositories configured, search only local database
    if repo_client.is_none() {
        if let Some(ref db) = db {
            // Search local database for packages matching pattern
            let local_packages = db.list_packages().unwrap_or_default();
            let matching_packages: Vec<_> = local_packages
                .into_iter()
                .filter(|pkg| pkg.name.contains(pattern) || pkg.description.contains(pattern))
                .collect();

            if matching_packages.is_empty() {
                println!("No packages found matching '{}'", pattern);
                return PostAction::Return;
            }

            println!("Found {} package(s)", matching_packages.len());
            for pkg in matching_packages {
                println!("  \x1B[33m{}\x1B[0m {}", pkg.name, pkg.version);
                if !pkg.description.is_empty() {
                    let desc = if pkg.description.len() > 70 {
                        format!("{}...", &pkg.description[..67])
                    } else {
                        pkg.description.clone()
                    };
                    println!("    {}", desc);
                }
                println!();
            }
        } else {
            println!("No packages found matching '{}' (database not available)", pattern);
        }
        return PostAction::Return;
    }

    // Process repository search results
    if results.is_empty() {
        println!("No packages found matching '{}'", pattern);
        return PostAction::Return;
    }

    let mut total_found = 0;
    for (source, packages) in results {
        println!("\x1B[36m{}:\x1B[0m", source);

        for pkg in packages {
            total_found += 1;

            // Check if installed
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

            // Truncate long descriptions
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

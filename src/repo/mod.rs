use commands::Command;
use flags::Flag;
use settings::{OriginKind, SettingsYaml, check_root_required};
use statebox::StateBox;
use utils::{PostAction, get_dir};
use std::fs::OpenOptions;
use std::path::Path;
use serde_json::json;

pub fn build(hierarchy: &[String]) -> Command {
    let list = Flag::new(
        Some('l'),
        "list",
        "List all configured repositories",
        false,
        false,
        |states, _| {
            states.shove("list_repos", true);
        },
    );

    let test = Flag::new(
        Some('t'),
        "test",
        "Test repository connectivity",
        true,
        false,
        |states, arg| {
            if let Some(repo_url) = arg {
                states.shove("test_repo", repo_url.clone());
            }
        },
    );

    let add = Flag::new(
        Some('a'),
        "add",
        "Add a new repository (URL should be provided as a positional argument)",
        false,
        false,
        |states, _| {
            states.shove("add_repo", true);
        },
    );

    let no_keyring = Flag::new(
        None,
        "no-keyring",
        "Skip keyring verification for this repository",
        false,
        false,
        |states, _| {
            states.shove("no_keyring", true);
        },
    );

    let pax_flag = Flag::new(
        None,
        "pax",
        "Specify that the repository is a PAX repository",
        false,
        false,
        |states, _| {
            states.shove("repo_type", "pax".to_string());
        },
    );

    let deb_flag = Flag::new(
        None,
        "deb",
        "Specify that the repository is a Debian repository",
        false,
        false,
        |states, _| {
            states.shove("repo_type", "deb".to_string());
        },
    );

    let rpm_flag = Flag::new(
        None,
        "rpm",
        "Specify that the repository is an RPM repository",
        false,
        false,
        |states, _| {
            states.shove("repo_type", "rpm".to_string());
        },
    );

    let remove = Flag::new(
        Some('r'),
        "remove",
        "Remove a repository (by index number or URL)",
        true,
        false,
        |states, arg| {
            if let Some(repo_identifier) = arg {
                states.shove("remove_repo", repo_identifier.clone());
            }
        },
    );

    Command::new(
        "repo",
        vec![String::from("repositories")],
        "Manage package repositories",
        vec![list, test, add, remove, no_keyring, pax_flag, deb_flag, rpm_flag],
        None,
        run,
        hierarchy,
    )
}

fn run(states: &StateBox, args: Option<&[String]>) -> PostAction {
    let mut settings = match SettingsYaml::get_settings() {
        Ok(settings) => settings,
        Err(fault) => return PostAction::Fuck(fault),
    };

    if states.get::<bool>("list_repos").is_some_and(|x| *x) {
        return list_repositories(&settings);
    }

    if let Some(repo_url) = states.get::<String>("test_repo") {
        return test_repository(repo_url);
    }

    if states.get::<bool>("add_repo").is_some_and(|x| *x) {
        // Check if we need root for adding repositories
        if let Some(action) = check_root_required(true) {
            return action;
        }
        
        // Get URL from positional arguments
        let repo_url = match args {
            Some(args) if !args.is_empty() => args[0].clone(),
            _ => {
                println!("\x1B[91mError: Repository URL is required\x1B[0m");
                println!("\x1B[90mUsage: pax repo -a [--pax|--rpm|--deb] <URL>\x1B[0m");
                println!("\x1B[90mExample: pax repo -a --rpm https://dl.fedoraproject.org/pub/fedora/linux/releases/43/Everything/x86_64/\x1B[0m");
                return PostAction::Fuck("Repository URL is required".to_string());
            }
        };
        
        let repo_type = states.get::<String>("repo_type").map(|s| s.as_str());
        return add_repository(&mut settings, &repo_url, repo_type, states.get::<bool>("no_keyring").copied().unwrap_or(false));
    }

    if let Some(repo_identifier) = states.get::<String>("remove_repo") {
        // #region agent log
        let _ = write_debug_log(&json!({
            "location": "src/repo/mod.rs:136",
            "message": "repo remove command triggered",
            "data": {
                "identifier": repo_identifier,
                "sources_count": settings.sources.len()
            },
            "timestamp": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis(),
            "sessionId": "debug-session",
            "runId": "run1",
            "hypothesisId": "A"
        }));
        // #endregion
        // Check if we need root for removing repositories
        if let Some(action) = check_root_required(true) {
            return action;
        }
        return remove_repository(&mut settings, repo_identifier);
    }

    // Default to listing repositories if no specific action requested
    list_repositories(&settings)
}

fn list_repositories(settings: &SettingsYaml) -> PostAction {
    if settings.sources.is_empty() && settings.mirror_list.is_none() {
        println!("\x1B[95mNo repositories configured\x1B[0m");
        println!("\x1B[90mPopulate /etc/pax/sources.conf to configure repositories/mirrors.\x1B[0m");
        return PostAction::Return;
    }

    println!("\x1B[92mConfigured Repositories:\x1B[0m");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!();

    // If mirror_list is configured, fetch the current best mirror URL to display
    let current_mirror_url = if settings.mirror_list.is_some() {
        match settings::get_best_mirror_url() {
            Ok(url) => Some(url),
            Err(_) => None,
        }
    } else {
        None
    };

    if let Some(list) = &settings.mirror_list {
        println!("\x1B[94mMirror List\x1B[0m");
        println!("   \x1B[90mURL:\x1B[0m {}", list);
        if let Some(current_url) = &current_mirror_url {
            println!("   \x1B[90mCurrent Mirror:\x1B[0m {}", current_url);
        }
        println!();
    }

    for (i, source) in settings.sources.iter().enumerate() {
        let (repo_type, url) = match source {
            OriginKind::Pax(url) => {
                // If this is a mirror-based PAX repo and we have the current mirror URL, show that instead
                if let Some(current_url) = &current_mirror_url {
                    // Check if this source URL matches the pattern of a mirror-based repo
                    // (e.g., contains "mirrors.oreonhq.com" or matches the mirror list pattern)
                    if url.contains("mirrors.oreonhq.com") || 
                       (settings.mirror_list.is_some() && url.contains("oreon")) {
                        ("PAX", current_url.clone())
                    } else {
                        ("PAX", url.clone())
                    }
                } else {
                    ("PAX", url.clone())
                }
            },
            OriginKind::Github { user, repo } => ("GitHub", format!("https://github.com/{}/{}", user, repo)),
            OriginKind::Apt(url) => ("APT", format!("apt://{}", url)),
            OriginKind::Rpm(url) => ("RPM", format!("rpm://{}", url)),
            OriginKind::CloudflareR2 { bucket, account_id, .. } => {
                ("Cloudflare R2", format!("r2://{}.{}", bucket, account_id))
            },
            OriginKind::Deb(url) => ("DEB", format!("deb://{}", url)),
            OriginKind::Yum(url) => ("YUM", format!("yum://{}", url)),
            OriginKind::LocalDir(path) => ("Local Directory", format!("file://{}", path)),
        };

        println!("\x1B[94m{}. {}\x1B[0m", i + 1, repo_type);
        println!("   \x1B[90mURL:\x1B[0m {}", url);
        println!();
    }

    println!("\x1B[90mTotal: {} repository(ies)\x1B[0m", settings.sources.len());
    PostAction::Return
}

fn add_repository(settings: &mut SettingsYaml, repo_url: &str, repo_type: Option<&str>, no_keyring: bool) -> PostAction {
    // Validate URL format
    if !is_valid_url(repo_url) {
        println!("\x1B[91mError: Invalid URL format: {}\x1B[0m", repo_url);
        println!("\x1B[90mURLs must start with http://, https://, or file://\x1B[0m");
        return PostAction::Fuck(format!("Invalid URL format: {}", repo_url));
    }
    
    println!("\x1B[94mAdding repository...\x1B[0m");
    println!("  \x1B[90mURL:\x1B[0m {}", repo_url);
    
    if let Some(rt) = repo_type {
        println!("  \x1B[90mType:\x1B[0m {}", rt);
    } else {
        println!("  \x1B[90mType:\x1B[0m (will infer from URL)");
    }

    // Clean the URL - remove type prefixes if they exist
    let clean_url = repo_url
        .strip_prefix("pax://")
        .or_else(|| repo_url.strip_prefix("apt://"))
        .or_else(|| repo_url.strip_prefix("deb://"))
        .or_else(|| repo_url.strip_prefix("rpm://"))
        .or_else(|| repo_url.strip_prefix("yum://"))
        .or_else(|| repo_url.strip_prefix("dnf://"))
        .unwrap_or(repo_url);
    
    let clean_url_trimmed = clean_url.trim_end_matches('/');

    // Test repository connectivity first
    let test_url = if clean_url.starts_with("https://github.com/") {
        format!("{}/releases", clean_url_trimmed)
    } else if repo_type == Some("deb") || repo_type == Some("apt") {
        format!("{}/Packages", clean_url_trimmed)
    } else if repo_type == Some("rpm") {
        format!("{}/repodata/repomd.xml", clean_url_trimmed)
    } else {
        format!("{}/packages.json", clean_url_trimmed)
    };

    println!("  \x1B[90mTesting connectivity...\x1B[0m");
    match reqwest::blocking::Client::new()
        .get(&test_url)
        .timeout(std::time::Duration::from_secs(10))
        .send() {
        Ok(response) => {
            if !response.status().is_success() {
                println!("\x1B[91mError: Repository responded with status: {}\x1B[0m", response.status());
                println!("\x1B[90mThe repository may not be accessible or the URL may be incorrect.\x1B[0m");
                return PostAction::Fuck(format!("Repository test failed with status: {}", response.status()));
            } else {
                println!("  \x1B[92m✓ Repository is accessible\x1B[0m");
            }
        }
        Err(e) => {
            println!("\x1B[91mError: Failed to connect to repository: {}\x1B[0m", e);
            println!("\x1B[90mPlease verify the URL is correct and the repository is accessible.\x1B[0m");
            return PostAction::Fuck(format!("Failed to connect to repository: {}", e));
        }
    }

    // Check if keyring verification is required
    if !no_keyring {
        println!("  \x1B[90mKeyring verification:\x1B[0m \x1B[93mNot yet supported for custom repositories\x1B[0m");
    }

    // Determine repository type - use explicit type if provided, otherwise infer from URL
    let origin_kind = if let Some(explicit_type) = repo_type {
        match explicit_type {
            "pax" => OriginKind::Pax(clean_url_trimmed.to_string()),
            "deb" => OriginKind::Deb(clean_url_trimmed.to_string()),
            "rpm" => OriginKind::Rpm(clean_url_trimmed.to_string()),
            _ => {
                println!("\x1B[91mInvalid repository type: {}\x1B[0m", explicit_type);
                return PostAction::Fuck(format!("Invalid repository type: {}", explicit_type));
            }
        }
    } else if clean_url.starts_with("https://github.com/") {
        if let Some((user, repo)) = clean_url_trimmed
            .strip_prefix("https://github.com/")
            .and_then(|s| s.split_once('/'))
        {
            OriginKind::Github {
                user: user.to_string(),
                repo: repo.to_string(),
            }
        } else {
            println!("\x1B[91mInvalid GitHub repository URL format\x1B[0m");
            return PostAction::Fuck("Invalid GitHub repository URL".to_string());
        }
    } else if repo_url.starts_with("apt://") {
        OriginKind::Apt(clean_url_trimmed.to_string())
    } else if repo_url.starts_with("deb://") {
        OriginKind::Deb(clean_url_trimmed.to_string())
    } else if repo_url.starts_with("yum://") || repo_url.starts_with("dnf://") {
        OriginKind::Yum(clean_url_trimmed.to_string())
    } else if repo_url.starts_with("rpm://") {
        OriginKind::Rpm(clean_url_trimmed.to_string())
    } else {
        // Default to Pax repository
        OriginKind::Pax(clean_url_trimmed.to_string())
    };

    // Check if repository already exists
    if settings.sources.iter().any(|existing| match (existing, &origin_kind) {
        (OriginKind::Pax(existing_url), OriginKind::Pax(new_url)) => existing_url == new_url,
        (OriginKind::Github { user: eu, repo: er }, OriginKind::Github { user: nu, repo: nr }) => eu == nu && er == nr,
        (OriginKind::Apt(eu), OriginKind::Apt(nu)) => eu == nu,
        (OriginKind::Deb(eu), OriginKind::Deb(nu)) => eu == nu,
        (OriginKind::Yum(eu), OriginKind::Yum(nu)) => eu == nu,
        (OriginKind::Rpm(eu), OriginKind::Rpm(nu)) => eu == nu,
        _ => false,
    }) {
        println!("\x1B[93mWarning: Repository already exists\x1B[0m");
        return PostAction::Return;
    }

    // Add the repository
    settings.sources.push(origin_kind);

    // Save settings
    match settings.clone().set_settings() {
        Ok(_) => {
            println!("\x1B[92m✓ Repository added successfully\x1B[0m");
            if no_keyring {
                println!("  \x1B[93mNote: Keyring verification is disabled for this repository\x1B[0m");
            }
            PostAction::Return
        }
        Err(e) => {
            println!("\x1B[91mError: Failed to save repository configuration: {}\x1B[0m", e);
            PostAction::Fuck(format!("Failed to save settings: {}", e))
        }
    }
}

/// Validate URL format
fn is_valid_url(url: &str) -> bool {
    url.starts_with("http://") || 
    url.starts_with("https://") || 
    url.starts_with("file://") ||
    url.starts_with("pax://") ||
    url.starts_with("apt://") ||
    url.starts_with("deb://") ||
    url.starts_with("rpm://") ||
    url.starts_with("yum://") ||
    url.starts_with("dnf://")
}

// #region agent log
fn write_debug_log(log_entry: &serde_json::Value) -> Result<(), ()> {
    let log_path = "/home/blester/pax-rs/.cursor/debug.log";
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(log_path) {
        if let Ok(json_str) = serde_json::to_string(log_entry) {
            use std::io::Write;
            let _ = writeln!(file, "{}", json_str);
        }
    }
    Ok(())
}
// #endregion

fn remove_from_sources_conf(path: &Path, url_to_remove: &str) -> Result<(), String> {
    use std::fs;
    use std::io::Write;
    
    let contents = fs::read_to_string(path)
        .map_err(|e| format!("Failed to read sources.conf: {}", e))?;
    
    // Clean the URL to remove - strip protocol prefixes and trailing slashes
    let clean_url_to_remove = url_to_remove
        .strip_prefix("rpm://")
        .or_else(|| url_to_remove.strip_prefix("yum://"))
        .or_else(|| url_to_remove.strip_prefix("dnf://"))
        .or_else(|| url_to_remove.strip_prefix("apt://"))
        .or_else(|| url_to_remove.strip_prefix("deb://"))
        .or_else(|| url_to_remove.strip_prefix("pax://"))
        .or_else(|| url_to_remove.strip_prefix("file://"))
        .unwrap_or(url_to_remove)
        .trim_end_matches('/')
        .to_string();
    
    let mut new_lines = Vec::new();
    let mut removed_any = false;
    
    for line in contents.lines() {
        let trimmed = line.trim();
        // Keep comments and empty lines
        if trimmed.is_empty() || trimmed.starts_with('#') {
            new_lines.push(line.to_string());
            continue;
        }
        
        // Parse the line to extract URL
        let mut should_remove = false;
        for part in trimmed.split_whitespace() {
            if let Some((key, value)) = part.split_once('=') {
                if key.trim().to_lowercase() == "url" {
                    // Clean the value from the file
                    let clean_value = value
                        .trim_matches(|c| matches!(c, '"' | '\''))
                        .strip_prefix("rpm://")
                        .or_else(|| value.strip_prefix("yum://"))
                        .or_else(|| value.strip_prefix("dnf://"))
                        .or_else(|| value.strip_prefix("apt://"))
                        .or_else(|| value.strip_prefix("deb://"))
                        .or_else(|| value.strip_prefix("pax://"))
                        .or_else(|| value.strip_prefix("file://"))
                        .unwrap_or(value)
                        .trim_matches(|c| matches!(c, '"' | '\''))
                        .trim_end_matches('/');
                    
                    // Match with or without trailing slash
                    if clean_value == clean_url_to_remove || 
                       clean_value.trim_end_matches('/') == clean_url_to_remove ||
                       clean_value == clean_url_to_remove.trim_end_matches('/') {
                        should_remove = true;
                        break;
                    }
                }
            }
        }
        
        // Also check if the line contains the URL as a substring (for simpler formats)
        if !should_remove && trimmed.contains(&clean_url_to_remove) {
            should_remove = true;
        }
        
        if should_remove {
            removed_any = true;
            // Skip this line (don't add it to new_lines)
        } else {
            new_lines.push(line.to_string());
        }
    }
    
    if removed_any {
        let new_contents = new_lines.join("\n");
        let mut file = fs::File::create(path)
            .map_err(|e| format!("Failed to write sources.conf: {}", e))?;
        file.write_all(new_contents.as_bytes())
            .map_err(|e| format!("Failed to write sources.conf: {}", e))?;
    }
    
    Ok(())
}

fn remove_repository(settings: &mut SettingsYaml, repo_identifier: &str) -> PostAction {
    // #region agent log
    let _ = write_debug_log(&json!({
        "location": "src/repo/mod.rs:remove_repository:entry",
        "message": "remove_repository called",
        "data": {
            "identifier": repo_identifier,
            "sources_count_before": settings.sources.len(),
            "sources": settings.sources.iter().enumerate().map(|(i, s)| {
                let (repo_type, url) = match s {
                    OriginKind::Pax(url) => ("PAX", url.clone()),
                    OriginKind::Github { user, repo } => ("GitHub", format!("https://github.com/{}/{}", user, repo)),
                    OriginKind::Apt(url) => ("APT", format!("apt://{}", url)),
                    OriginKind::Rpm(url) => ("RPM", format!("rpm://{}", url)),
                    OriginKind::CloudflareR2 { bucket, account_id, .. } => {
                        ("Cloudflare R2", format!("r2://{}.{}", bucket, account_id))
                    },
                    OriginKind::Deb(url) => ("DEB", format!("deb://{}", url)),
                    OriginKind::Yum(url) => ("YUM", format!("yum://{}", url)),
                    OriginKind::LocalDir(path) => ("Local Directory", format!("file://{}", path)),
                };
                json!({"index": i + 1, "type": repo_type, "url": url})
            }).collect::<Vec<_>>()
        },
        "timestamp": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis(),
        "sessionId": "debug-session",
        "runId": "run1",
        "hypothesisId": "A"
    }));
    // #endregion
    
    let removed: Option<OriginKind>;
    // Try to parse as index number first
    if let Ok(index) = repo_identifier.parse::<usize>() {
        // #region agent log
        let _ = write_debug_log(&json!({
            "location": "src/repo/mod.rs:remove_repository:index_parse",
            "message": "parsed as index",
            "data": {
                "index": index,
                "sources_len": settings.sources.len()
            },
            "timestamp": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis(),
            "sessionId": "debug-session",
            "runId": "run1",
            "hypothesisId": "B"
        }));
        // #endregion
        if index == 0 || index > settings.sources.len() {
            println!("\x1B[91mInvalid repository index: {}\x1B[0m", index);
            println!("\x1B[90mUse 'pax repo -l' to see available repositories\x1B[0m");
            return PostAction::Fuck(format!("Invalid repository index: {}", index));
        }
        
        removed = Some(settings.sources.remove(index - 1));
        let (repo_type, url) = match removed.as_ref().unwrap() {
            OriginKind::Pax(url) => ("PAX", url.clone()),
            OriginKind::Github { user, repo } => ("GitHub", format!("https://github.com/{}/{}", user, repo)),
            OriginKind::Apt(url) => ("APT", format!("apt://{}", url)),
            OriginKind::Rpm(url) => ("RPM", format!("rpm://{}", url)),
            OriginKind::CloudflareR2 { bucket, account_id, .. } => {
                ("Cloudflare R2", format!("r2://{}.{}", bucket, account_id))
            },
            OriginKind::Deb(url) => ("DEB", format!("deb://{}", url)),
            OriginKind::Yum(url) => ("YUM", format!("yum://{}", url)),
            OriginKind::LocalDir(path) => ("Local Directory", format!("file://{}", path)),
        };
        
        println!("\x1B[92mRemoved repository:\x1B[0m");
        println!("   \x1B[94mType:\x1B[0m {}", repo_type);
        println!("   \x1B[94mURL:\x1B[0m {}", url);
        // #region agent log
        let _ = write_debug_log(&json!({
            "location": "src/repo/mod.rs:remove_repository:index_removed",
            "message": "repository removed by index",
            "data": {
                "index": index,
                "repo_type": repo_type,
                "url": url,
                "sources_count_after": settings.sources.len()
            },
            "timestamp": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis(),
            "sessionId": "debug-session",
            "runId": "run1",
            "hypothesisId": "B"
        }));
        // #endregion
    } else {
        // #region agent log
        let _ = write_debug_log(&json!({
            "location": "src/repo/mod.rs:remove_repository:url_match",
            "message": "trying URL match",
            "data": {
                "identifier": repo_identifier
            },
            "timestamp": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis(),
            "sessionId": "debug-session",
            "runId": "run1",
            "hypothesisId": "C"
        }));
        // #endregion
        // Try to match by URL
        let clean_identifier = repo_identifier
            .strip_prefix("pax://")
            .or_else(|| repo_identifier.strip_prefix("apt://"))
            .or_else(|| repo_identifier.strip_prefix("deb://"))
            .or_else(|| repo_identifier.strip_prefix("rpm://"))
            .or_else(|| repo_identifier.strip_prefix("yum://"))
            .or_else(|| repo_identifier.strip_prefix("dnf://"))
            .unwrap_or(repo_identifier)
            .trim_end_matches('/')
            .to_string();
        // #region agent log
        let _ = write_debug_log(&json!({
            "location": "src/repo/mod.rs:remove_repository:clean_identifier",
            "message": "cleaned identifier",
            "data": {
                "original": repo_identifier,
                "cleaned": clean_identifier
            },
            "timestamp": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis(),
            "sessionId": "debug-session",
            "runId": "run1",
            "hypothesisId": "C"
        }));
        // #endregion
        let mut removed_index = None;
        
        for (i, source) in settings.sources.iter().enumerate() {
            let matches = match source {
                OriginKind::Pax(url) => {
                    url.trim_end_matches('/') == clean_identifier || 
                    repo_identifier.trim_end_matches('/') == url.trim_end_matches('/')
                },
                OriginKind::Github { user, repo } => {
                    let github_url = format!("https://github.com/{}/{}", user, repo);
                    github_url.trim_end_matches('/') == clean_identifier ||
                    repo_identifier.contains(user) && repo_identifier.contains(repo)
                },
                OriginKind::Apt(url) | OriginKind::Deb(url) | OriginKind::Rpm(url) | OriginKind::Yum(url) => {
                    url.trim_end_matches('/') == clean_identifier ||
                    repo_identifier.trim_end_matches('/') == url.trim_end_matches('/')
                },
                OriginKind::CloudflareR2 { bucket, account_id, .. } => {
                    let r2_url = format!("r2://{}.{}", bucket, account_id);
                    r2_url == repo_identifier
                },
                OriginKind::LocalDir(path) => {
                    path == repo_identifier || repo_identifier == format!("file://{}", path)
                },
            };
            
            if matches {
                // #region agent log
                let _ = write_debug_log(&json!({
                    "location": "src/repo/mod.rs:remove_repository:match_found",
                    "message": "repository match found",
                    "data": {
                        "index": i,
                        "source_type": format!("{:?}", source)
                    },
                    "timestamp": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis(),
                    "sessionId": "debug-session",
                    "runId": "run1",
                    "hypothesisId": "C"
                }));
                // #endregion
                removed_index = Some(i);
                break;
            }
        }
        // #region agent log
        let _ = write_debug_log(&json!({
            "location": "src/repo/mod.rs:remove_repository:match_result",
            "message": "match search complete",
            "data": {
                "removed_index": removed_index,
                "searched_count": settings.sources.len()
            },
            "timestamp": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis(),
            "sessionId": "debug-session",
            "runId": "run1",
            "hypothesisId": "C"
        }));
        // #endregion
        if let Some(index) = removed_index {
            removed = Some(settings.sources.remove(index));
            let (repo_type, url) = match removed.as_ref().unwrap() {
                OriginKind::Pax(url) => ("PAX", url.clone()),
                OriginKind::Github { user, repo } => ("GitHub", format!("https://github.com/{}/{}", user, repo)),
                OriginKind::Apt(url) => ("APT", format!("apt://{}", url)),
                OriginKind::Rpm(url) => ("RPM", format!("rpm://{}", url)),
                OriginKind::CloudflareR2 { bucket, account_id, .. } => {
                    ("Cloudflare R2", format!("r2://{}.{}", bucket, account_id))
                },
                OriginKind::Deb(url) => ("DEB", format!("deb://{}", url)),
                OriginKind::Yum(url) => ("YUM", format!("yum://{}", url)),
                OriginKind::LocalDir(path) => ("Local Directory", format!("file://{}", path)),
            };
            
            println!("\x1B[92mRemoved repository:\x1B[0m");
            println!("   \x1B[94mType:\x1B[0m {}", repo_type);
            println!("   \x1B[94mURL:\x1B[0m {}", url);
            // #region agent log
            let _ = write_debug_log(&json!({
                "location": "src/repo/mod.rs:remove_repository:url_removed",
                "message": "repository removed by URL",
                "data": {
                    "index": index,
                    "repo_type": repo_type,
                    "url": url,
                    "sources_count_after": settings.sources.len()
                },
                "timestamp": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis(),
                "sessionId": "debug-session",
                "runId": "run1",
                "hypothesisId": "C"
            }));
            // #endregion
        } else {
            // #region agent log
            let _ = write_debug_log(&json!({
                "location": "src/repo/mod.rs:remove_repository:not_found",
                "message": "repository not found",
                "data": {
                    "identifier": repo_identifier,
                    "clean_identifier": clean_identifier,
                    "sources_count": settings.sources.len()
                },
                "timestamp": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis(),
                "sessionId": "debug-session",
                "runId": "run1",
                "hypothesisId": "D"
            }));
            // #endregion
            println!("\x1B[91mRepository not found: {}\x1B[0m", repo_identifier);
            println!("\x1B[90mUse 'pax repo -l' to see available repositories\x1B[0m");
            return PostAction::Fuck(format!("Repository not found: {}", repo_identifier));
        }
    }
    
    // Also remove from sources.conf if it exists (it takes precedence over YAML)
    let removed = removed.unwrap(); // Safe because we only reach here if removal succeeded
    let removed_url = match &removed {
        OriginKind::Pax(url) => Some(url.clone()),
        OriginKind::Rpm(url) => Some(url.clone()),
        OriginKind::Apt(url) => Some(url.clone()),
        OriginKind::Deb(url) => Some(url.clone()),
        OriginKind::Yum(url) => Some(url.clone()),
        OriginKind::Github { user, repo } => Some(format!("https://github.com/{}/{}", user, repo)),
        OriginKind::CloudflareR2 { bucket, account_id, .. } => Some(format!("r2://{}.{}", bucket, account_id)),
        OriginKind::LocalDir(path) => Some(format!("file://{}", path)),
    };
    
    if let Some(url_to_remove) = &removed_url {
        // Add to disabled_sources to prevent automatic re-addition
        let clean_url = url_to_remove
            .strip_prefix("rpm://")
            .or_else(|| url_to_remove.strip_prefix("yum://"))
            .or_else(|| url_to_remove.strip_prefix("dnf://"))
            .or_else(|| url_to_remove.strip_prefix("apt://"))
            .or_else(|| url_to_remove.strip_prefix("deb://"))
            .or_else(|| url_to_remove.strip_prefix("pax://"))
            .or_else(|| url_to_remove.strip_prefix("file://"))
            .unwrap_or(url_to_remove)
            .trim_end_matches('/')
            .to_string();
        
        if !settings.disabled_sources.contains(&clean_url) {
            settings.disabled_sources.push(clean_url.clone());
        }
        
        // Also try to remove from sources.conf if it exists
        if let Ok(dir) = get_dir() {
            let sources_conf_path = dir.join("sources.conf");
            if sources_conf_path.exists() {
                if let Err(e) = remove_from_sources_conf(&sources_conf_path, &clean_url) {
                    println!("\x1B[93mWarning: Failed to update sources.conf: {}\x1B[0m", e);
                }
            }
        }
    }
    
    // Save settings
    // #region agent log
    let _ = write_debug_log(&json!({
        "location": "src/repo/mod.rs:remove_repository:before_save",
        "message": "attempting to save settings",
        "data": {
            "sources_count": settings.sources.len()
        },
        "timestamp": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis(),
        "sessionId": "debug-session",
        "runId": "run1",
        "hypothesisId": "E"
    }));
    // #endregion
    match settings.clone().set_settings() {
        Ok(_) => {
            // #region agent log
            let _ = write_debug_log(&json!({
                "location": "src/repo/mod.rs:remove_repository:save_success",
                "message": "settings saved successfully",
                "data": {
                    "sources_count": settings.sources.len()
                },
                "timestamp": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis(),
                "sessionId": "debug-session",
                "runId": "run1",
                "hypothesisId": "E"
            }));
            // #endregion
            println!("\x1B[92mRepository removed successfully\x1B[0m");
            PostAction::Return
        }
        Err(e) => {
            // #region agent log
            let _ = write_debug_log(&json!({
                "location": "src/repo/mod.rs:remove_repository:save_error",
                "message": "settings save failed",
                "data": {
                    "error": e,
                    "sources_count": settings.sources.len()
                },
                "timestamp": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis(),
                "sessionId": "debug-session",
                "runId": "run1",
                "hypothesisId": "E"
            }));
            // #endregion
            println!("\x1B[91mFailed to save repository configuration: {}\x1B[0m", e);
            PostAction::Fuck(format!("Failed to save settings: {}", e))
        }
    }
}

fn test_repository(repo_url: &str) -> PostAction {
    println!("Testing repository connectivity: {}", repo_url);

    // Simple connectivity test
    let test_url = if repo_url.starts_with("https://github.com/") {
        format!("{}/releases", repo_url)
    } else if repo_url.starts_with("apt://") {
        let apt_url = repo_url.strip_prefix("apt://").unwrap();
        format!("{}/Packages", apt_url)
    } else if repo_url.starts_with("rpm://") {
        let rpm_url = repo_url.strip_prefix("rpm://").unwrap();
        format!("{}/repodata/repomd.xml", rpm_url)
    } else {
        format!("{}/health", repo_url)
    };

    match reqwest::blocking::get(&test_url) {
        Ok(response) => {
            if response.status().is_success() {
                println!("\x1B[92mRepository is accessible\x1B[0m");
            } else {
                println!("\x1B[93mRepository responded with status: {}\x1B[0m", response.status());
            }
        }
        Err(e) => {
            println!("\x1B[91mFailed to connect to repository: {}\x1B[0m", e);
        }
    }

    PostAction::Return
}

use crate::adapters::Dependency;
use crate::database::Database;
use crate::provides::ProvidesManager;
use std::collections::{HashMap, HashSet};

// Dependency resolver with topological sorting
pub struct DependencyResolver {
    db: Database,
    provides_mgr: ProvidesManager,
}

#[derive(Debug, Clone)]
pub struct ResolvedPackage {
    pub name: String,
    pub version: Option<String>,
    pub install_order: usize,
}

impl DependencyResolver {
    // Create new dependency resolver
    pub fn new(db: Database) -> Self {
        let provides_mgr = ProvidesManager::new(db.clone());
        
        DependencyResolver {
            db,
            provides_mgr,
        }
    }

    // Resolve dependencies for a list of packages
    pub fn resolve(
        &self,
        packages: &[String],
        available_packages: &HashMap<String, PackageInfo>,
    ) -> Result<Vec<ResolvedPackage>, String> {
        let mut resolved = Vec::new();
        let mut visited = HashSet::new();
        let mut order_counter = 0;

        for package in packages {
            self.resolve_recursive(
                package,
                available_packages,
                &mut resolved,
                &mut visited,
                &mut order_counter,
            )?;
        }

        // Sort by install order
        resolved.sort_by_key(|p| p.install_order);

        Ok(resolved)
    }

    // Recursively resolve dependencies
    fn resolve_recursive(
        &self,
        package_name: &str,
        available_packages: &HashMap<String, PackageInfo>,
        resolved: &mut Vec<ResolvedPackage>,
        visited: &mut HashSet<String>,
        order_counter: &mut usize,
    ) -> Result<(), String> {
        // Skip if already visited
        if visited.contains(package_name) {
            return Ok(());
        }

        // Check if already installed
        if self.db.is_installed(package_name)
            .map_err(|e| format!("Database error: {}", e))? {
            visited.insert(package_name.to_string());
            return Ok(());
        }

        // Check if dependency is satisfied by system
        if self.provides_mgr.is_satisfied(package_name)? {
            visited.insert(package_name.to_string());
            return Ok(());
        }

        // Find package info
        let pkg_info = available_packages.get(package_name)
            .ok_or_else(|| format!("Package not found: {}", package_name))?;

        visited.insert(package_name.to_string());

        // Resolve dependencies first (depth-first)
        for dep in &pkg_info.dependencies {
            // Try to resolve dependency
            let dep_name = self.resolve_dependency_name(&dep.name, available_packages)?;
            
            self.resolve_recursive(
                &dep_name,
                available_packages,
                resolved,
                visited,
                order_counter,
            )?;
        }

        // Add this package after its dependencies
        resolved.push(ResolvedPackage {
            name: package_name.to_string(),
            version: Some(pkg_info.version.clone()),
            install_order: *order_counter,
        });
        *order_counter += 1;

        Ok(())
    }

    // Resolve a dependency name to an actual package
    fn resolve_dependency_name(
        &self,
        dep_name: &str,
        available_packages: &HashMap<String, PackageInfo>,
    ) -> Result<String, String> {
        // Check if dependency is satisfied by an installed package
        if let Some(provider) = self.provides_mgr.find_provider(dep_name)? {
            return Ok(provider);
        }

        // Check if exact package exists
        if available_packages.contains_key(dep_name) {
            return Ok(dep_name.to_string());
        }

        // Try to find a package that provides this
        for (pkg_name, pkg_info) in available_packages {
            for provide in &pkg_info.provides {
                if provide == dep_name {
                    return Ok(pkg_name.clone());
                }
            }
        }

        // Check if it's a system dependency
        if self.provides_mgr.is_satisfied(dep_name)? {
            // Satisfied by system, no package needed
            return Err(format!("Dependency {} satisfied by system", dep_name));
        }

        Err(format!("Cannot resolve dependency: {}", dep_name))
    }

    // Check for circular dependencies
    pub fn check_circular(
        &self,
        packages: &HashMap<String, PackageInfo>,
    ) -> Result<(), String> {
        for (pkg_name, _pkg_info) in packages {
            let mut visited = HashSet::new();
            let mut stack = Vec::new();
            
            self.detect_circular(
                pkg_name,
                packages,
                &mut visited,
                &mut stack,
            )?;
        }

        Ok(())
    }

    // Recursive circular dependency detection
    fn detect_circular(
        &self,
        package_name: &str,
        available_packages: &HashMap<String, PackageInfo>,
        visited: &mut HashSet<String>,
        stack: &mut Vec<String>,
    ) -> Result<(), String> {
        if stack.contains(&package_name.to_string()) {
            stack.push(package_name.to_string());
            return Err(format!("Circular dependency detected: {}", stack.join(" -> ")));
        }

        if visited.contains(package_name) {
            return Ok(());
        }

        visited.insert(package_name.to_string());
        stack.push(package_name.to_string());

        if let Some(pkg_info) = available_packages.get(package_name) {
            for dep in &pkg_info.dependencies {
                if let Ok(dep_name) = self.resolve_dependency_name(&dep.name, available_packages) {
                    self.detect_circular(&dep_name, available_packages, visited, stack)?;
                }
            }
        }

        stack.pop();
        Ok(())
    }

    // Calculate what would be removed if a package is removed
    pub fn calculate_removal_impact(&self, package_name: &str) -> Result<Vec<String>, String> {
        let rdeps = self.db.get_reverse_dependencies(package_name)
            .map_err(|e| format!("Failed to get reverse dependencies: {}", e))?;

        Ok(rdeps)
    }

    // Validate version constraints
    pub fn validate_version_constraint(
        version: &str,
        constraint: &str,
    ) -> Result<bool, String> {
        // Simple version comparison
        // Format: >=1.0, <=2.0, =1.5, >1.0, <2.0
        
        if constraint.starts_with(">=") {
            let required = &constraint[2..].trim();
            Ok(compare_versions(version, required) >= 0)
        } else if constraint.starts_with("<=") {
            let required = &constraint[2..].trim();
            Ok(compare_versions(version, required) <= 0)
        } else if constraint.starts_with('>') {
            let required = &constraint[1..].trim();
            Ok(compare_versions(version, required) > 0)
        } else if constraint.starts_with('<') {
            let required = &constraint[1..].trim();
            Ok(compare_versions(version, required) < 0)
        } else if constraint.starts_with('=') {
            let required = constraint[1..].trim();
            Ok(version == required)
        } else {
            // no operator means exact match
            Ok(version == constraint.trim())
        }
    }
}

// Package info for resolver
#[derive(Debug, Clone)]
pub struct PackageInfo {
    pub version: String,
    pub dependencies: Vec<Dependency>,
    pub provides: Vec<String>,
}

// Simple version comparison
// Returns: -1 if v1 < v2, 0 if equal, 1 if v1 > v2
fn compare_versions(v1: &str, v2: &str) -> i32 {
    let parts1: Vec<&str> = v1.split('.').collect();
    let parts2: Vec<&str> = v2.split('.').collect();

    for i in 0..parts1.len().max(parts2.len()) {
        let p1 = parts1.get(i).and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
        let p2 = parts2.get(i).and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);

        if p1 < p2 {
            return -1;
        } else if p1 > p2 {
            return 1;
        }
    }

    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compare_versions() {
        assert_eq!(compare_versions("1.0", "1.0"), 0);
        assert_eq!(compare_versions("1.0", "2.0"), -1);
        assert_eq!(compare_versions("2.0", "1.0"), 1);
        assert_eq!(compare_versions("1.2.3", "1.2.4"), -1);
        assert_eq!(compare_versions("1.10", "1.9"), 1);
    }

    #[test]
    fn test_validate_version_constraint() {
        assert!(DependencyResolver::validate_version_constraint("1.5", ">=1.0").unwrap());
        assert!(DependencyResolver::validate_version_constraint("1.0", "<=2.0").unwrap());
        assert!(!DependencyResolver::validate_version_constraint("0.5", ">=1.0").unwrap());
        assert!(DependencyResolver::validate_version_constraint("1.5", "=1.5").unwrap());
    }
}


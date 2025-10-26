use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

use crate::{DepVer, InstalledMetaData};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConflictResolution {
    pub conflicts: Vec<PackageConflict>,
    pub solutions: Vec<ConflictSolution>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageConflict {
    pub package: String,
    pub conflict_type: ConflictType,
    pub conflicting_packages: Vec<String>,
    pub details: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ConflictType {
    FileConflict,
    DependencyConflict,
    VersionConflict,
    ServiceConflict,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConflictSolution {
    pub solution_type: SolutionType,
    pub packages_to_remove: Vec<String>,
    pub packages_to_install: Vec<String>,
    pub packages_to_upgrade: Vec<String>,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SolutionType {
    RemoveConflicting,
    UpgradeConflicting,
    ReplaceConflicting,
    SkipInstallation,
}

pub struct DependencyResolver {
    installed_packages: HashMap<String, InstalledMetaData>,
    requested_packages: Vec<DepVer>,
}

impl DependencyResolver {
    pub fn new() -> Self {
        Self {
            installed_packages: HashMap::new(),
            requested_packages: Vec::new(),
        }
    }

    pub fn add_installed_package(&mut self, package: InstalledMetaData) {
        self.installed_packages.insert(package.name.clone(), package);
    }

    pub fn add_requested_package(&mut self, package: DepVer) {
        self.requested_packages.push(package);
    }

    pub fn resolve_conflicts(&self) -> Result<ConflictResolution, String> {
        let mut conflicts = Vec::new();
        let mut solutions = Vec::new();

        // Check for file conflicts
        self.check_file_conflicts(&mut conflicts)?;
        
        // Check for dependency conflicts
        self.check_dependency_conflicts(&mut conflicts)?;
        
        // Check for version conflicts
        self.check_version_conflicts(&mut conflicts)?;

        // Generate solutions for each conflict
        for conflict in &conflicts {
            solutions.extend(self.generate_solutions(conflict)?);
        }

        Ok(ConflictResolution {
            conflicts,
            solutions,
        })
    }

    fn check_file_conflicts(&self, conflicts: &mut Vec<PackageConflict>) -> Result<(), String> {
        let mut file_owners: HashMap<String, String> = HashMap::new();

        // Check installed packages for file conflicts
        for name in self.installed_packages.keys() {
            if let Ok(manifest) = crate::file_tracking::FileManifest::load(name) {
                for file in &manifest.files {
                    if let Some(existing_owner) = file_owners.get(&file.path.to_string_lossy().to_string()) {
                        if existing_owner != name {
                            conflicts.push(PackageConflict {
                                package: name.to_string(),
                                conflict_type: ConflictType::FileConflict,
                                conflicting_packages: vec![existing_owner.clone()],
                                details: format!(
                                    "File {} is owned by both {} and {}",
                                    file.path.display(),
                                    existing_owner,
                                    name
                                ),
                            });
                        }
                    } else {
                        file_owners.insert(file.path.to_string_lossy().to_string(), name.to_string());
                    }
                }
            }
        }

        Ok(())
    }

    fn check_dependency_conflicts(&self, conflicts: &mut Vec<PackageConflict>) -> Result<(), String> {
        // Check for circular dependencies
        for name in self.installed_packages.keys() {
            let mut visited = HashSet::new();
            let mut stack = vec![name.clone()];
            
            while let Some(current) = stack.pop() {
                if visited.contains(&current) {
                    conflicts.push(PackageConflict {
                        package: name.to_string(),
                        conflict_type: ConflictType::DependencyConflict,
                        conflicting_packages: vec![current.clone()],
                        details: format!("Circular dependency detected involving {}", current),
                    });
                    break;
                }
                
                visited.insert(current.clone());
                
                if let Some(pkg) = self.installed_packages.get(&current) {
                    for dep in &pkg.dependencies {
                        stack.push(dep.name.clone());
                    }
                }
            }
        }

        Ok(())
    }

    fn check_version_conflicts(&self, conflicts: &mut Vec<PackageConflict>) -> Result<(), String> {
        // Check for version conflicts in requested packages
        let mut package_versions: HashMap<String, Vec<&DepVer>> = HashMap::new();
        
        for req_pkg in &self.requested_packages {
            package_versions.entry(req_pkg.name.clone())
                .or_insert_with(Vec::new)
                .push(req_pkg);
        }

        for (name, versions) in package_versions {
            if versions.len() > 1 {
                // Check if versions are compatible
                let mut incompatible = Vec::new();
                for (i, v1) in versions.iter().enumerate() {
                    for (j, v2) in versions.iter().enumerate() {
                        if i != j && !self.versions_compatible(v1, v2) {
                            incompatible.push(format!("{:?} vs {:?}", v1.range, v2.range));
                        }
                    }
                }
                
                if !incompatible.is_empty() {
                    conflicts.push(PackageConflict {
                        package: name,
                        conflict_type: ConflictType::VersionConflict,
                        conflicting_packages: Vec::new(),
                        details: format!("Incompatible version requirements: {}", incompatible.join(", ")),
                    });
                }
            }
        }

        Ok(())
    }

    fn versions_compatible(&self, v1: &DepVer, v2: &DepVer) -> bool {
        // Check if two version ranges are compatible
        // This is a simplified check - in reality, this would be more complex
        v1.range.lower == v2.range.lower && v1.range.upper == v2.range.upper
    }

    fn generate_solutions(&self, conflict: &PackageConflict) -> Result<Vec<ConflictSolution>, String> {
        let mut solutions = Vec::new();

        match conflict.conflict_type {
            ConflictType::FileConflict => {
                // Solution 1: Remove conflicting package
                solutions.push(ConflictSolution {
                    solution_type: SolutionType::RemoveConflicting,
                    packages_to_remove: conflict.conflicting_packages.clone(),
                    packages_to_install: Vec::new(),
                    packages_to_upgrade: Vec::new(),
                    description: format!("Remove conflicting package(s) to resolve file conflict"),
                });

                // Solution 2: Skip installation
                solutions.push(ConflictSolution {
                    solution_type: SolutionType::SkipInstallation,
                    packages_to_remove: Vec::new(),
                    packages_to_install: Vec::new(),
                    packages_to_upgrade: Vec::new(),
                    description: format!("Skip installation of {} to avoid file conflict", conflict.package),
                });
            }
            ConflictType::DependencyConflict => {
                solutions.push(ConflictSolution {
                    solution_type: SolutionType::RemoveConflicting,
                    packages_to_remove: vec![conflict.package.clone()],
                    packages_to_install: Vec::new(),
                    packages_to_upgrade: Vec::new(),
                    description: format!("Remove package with circular dependency"),
                });
            }
            ConflictType::VersionConflict => {
                solutions.push(ConflictSolution {
                    solution_type: SolutionType::SkipInstallation,
                    packages_to_remove: Vec::new(),
                    packages_to_install: Vec::new(),
                    packages_to_upgrade: Vec::new(),
                    description: format!("Skip installation due to version conflict"),
                });
            }
            ConflictType::ServiceConflict => {
                solutions.push(ConflictSolution {
                    solution_type: SolutionType::ReplaceConflicting,
                    packages_to_remove: conflict.conflicting_packages.clone(),
                    packages_to_install: vec![conflict.package.clone()],
                    packages_to_upgrade: Vec::new(),
                    description: format!("Replace conflicting service package"),
                });
            }
        }

        Ok(solutions)
    }

    pub fn apply_solution(&mut self, solution: &ConflictSolution) -> Result<(), String> {
        match solution.solution_type {
            SolutionType::RemoveConflicting => {
                for package_name in &solution.packages_to_remove {
                    self.installed_packages.remove(package_name);
                }
            }
            SolutionType::UpgradeConflicting => {
                // Implementation would upgrade packages
                println!("Upgrading packages: {:?}", solution.packages_to_upgrade);
            }
            SolutionType::ReplaceConflicting => {
                for package_name in &solution.packages_to_remove {
                    self.installed_packages.remove(package_name);
                }
                // Add new packages to install
                for package_name in &solution.packages_to_install {
                    // This would typically involve installing the package
                    println!("Installing package: {}", package_name);
                }
            }
            SolutionType::SkipInstallation => {
                // Remove from requested packages
                self.requested_packages.retain(|pkg| {
                    !solution.packages_to_install.contains(&pkg.name)
                });
            }
        }

        Ok(())
    }
}

impl Default for DependencyResolver {
    fn default() -> Self {
        Self::new()
    }
}

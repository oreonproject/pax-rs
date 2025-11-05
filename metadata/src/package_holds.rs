use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs::File,
    io::{Read, Write},
    time::{SystemTime, UNIX_EPOCH},
};

use utils::{err, get_metadata_dir, Version};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageHold {
    pub package_name: String,
    pub hold_type: HoldType,
    pub reason: String,
    pub created_at: u64,
    pub created_by: Option<String>,
    pub expires_at: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum HoldType {
    NoUpgrade,      // Prevent upgrades but allow downgrades
    NoDowngrade,    // Prevent downgrades but allow upgrades
    NoChange,       // Prevent any version changes
    VersionPin,     // Pin to specific version
    RepositoryPin,  // Pin to specific repository
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionPin {
    pub package_name: String,
    pub version: Version,
    pub reason: String,
    pub created_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepositoryPin {
    pub package_name: String,
    pub repository: String,
    pub reason: String,
    pub created_at: u64,
}

pub struct PackageHoldManager {
    holds: HashMap<String, PackageHold>,
    version_pins: HashMap<String, VersionPin>,
    repository_pins: HashMap<String, RepositoryPin>,
}

impl PackageHoldManager {
    pub fn new() -> Self {
        Self {
            holds: HashMap::new(),
            version_pins: HashMap::new(),
            repository_pins: HashMap::new(),
        }
    }

    pub fn hold_package(
        &mut self,
        package_name: String,
        hold_type: HoldType,
        reason: String,
        expires_at: Option<u64>,
    ) -> Result<(), String> {
        let hold = PackageHold {
            package_name: package_name.clone(),
            hold_type,
            reason,
            created_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            created_by: None, // Could be set to current user
            expires_at,
        };

        self.holds.insert(package_name, hold);
        self.save_holds()?;
        
        Ok(())
    }

    pub fn pin_version(
        &mut self,
        package_name: String,
        version: Version,
        reason: String,
    ) -> Result<(), String> {
        let pin = VersionPin {
            package_name: package_name.clone(),
            version,
            reason,
            created_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        };

        self.version_pins.insert(package_name, pin);
        self.save_version_pins()?;
        
        Ok(())
    }

    pub fn pin_repository(
        &mut self,
        package_name: String,
        repository: String,
        reason: String,
    ) -> Result<(), String> {
        let pin = RepositoryPin {
            package_name: package_name.clone(),
            repository,
            reason,
            created_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        };

        self.repository_pins.insert(package_name, pin);
        self.save_repository_pins()?;
        
        Ok(())
    }

    pub fn unhold_package(&mut self, package_name: &str) -> Result<(), String> {
        if self.holds.remove(package_name).is_some() {
            self.save_holds()?;
            println!("Removed hold on package: {}", package_name);
        } else {
            return err!("Package {} is not held", package_name);
        }
        
        Ok(())
    }

    pub fn unpin_version(&mut self, package_name: &str) -> Result<(), String> {
        if self.version_pins.remove(package_name).is_some() {
            self.save_version_pins()?;
            println!("Removed version pin on package: {}", package_name);
        } else {
            return err!("Package {} is not version pinned", package_name);
        }
        
        Ok(())
    }

    pub fn unpin_repository(&mut self, package_name: &str) -> Result<(), String> {
        if self.repository_pins.remove(package_name).is_some() {
            self.save_repository_pins()?;
            println!("Removed repository pin on package: {}", package_name);
        } else {
            return err!("Package {} is not repository pinned", package_name);
        }
        
        Ok(())
    }

    pub fn is_package_held(&self, package_name: &str) -> bool {
        self.holds.contains_key(package_name)
    }

    pub fn is_version_pinned(&self, package_name: &str) -> bool {
        self.version_pins.contains_key(package_name)
    }

    pub fn is_repository_pinned(&self, package_name: &str) -> bool {
        self.repository_pins.contains_key(package_name)
    }

    pub fn can_upgrade(&self, package_name: &str) -> bool {
        if let Some(hold) = self.holds.get(package_name) {
            if self.is_hold_expired(hold) {
                return true;
            }
            
            match hold.hold_type {
                HoldType::NoUpgrade | HoldType::NoChange => false,
                HoldType::NoDowngrade | HoldType::VersionPin | HoldType::RepositoryPin => true,
            }
        } else {
            true
        }
    }

    pub fn can_downgrade(&self, package_name: &str) -> bool {
        if let Some(hold) = self.holds.get(package_name) {
            if self.is_hold_expired(hold) {
                return true;
            }
            
            match hold.hold_type {
                HoldType::NoDowngrade | HoldType::NoChange => false,
                HoldType::NoUpgrade | HoldType::VersionPin | HoldType::RepositoryPin => true,
            }
        } else {
            true
        }
    }

    pub fn get_pinned_version(&self, package_name: &str) -> Option<&Version> {
        self.version_pins.get(package_name).map(|pin| &pin.version)
    }

    pub fn get_pinned_repository(&self, package_name: &str) -> Option<&String> {
        self.repository_pins.get(package_name).map(|pin| &pin.repository)
    }

    pub fn list_held_packages(&self) -> Vec<&PackageHold> {
        let mut holds: Vec<&PackageHold> = self.holds.values().collect();
        holds.sort_by(|a, b| a.package_name.cmp(&b.package_name));
        holds
    }

    pub fn list_version_pins(&self) -> Vec<&VersionPin> {
        let mut pins: Vec<&VersionPin> = self.version_pins.values().collect();
        pins.sort_by(|a, b| a.package_name.cmp(&b.package_name));
        pins
    }

    pub fn list_repository_pins(&self) -> Vec<&RepositoryPin> {
        let mut pins: Vec<&RepositoryPin> = self.repository_pins.values().collect();
        pins.sort_by(|a, b| a.package_name.cmp(&b.package_name));
        pins
    }

    pub fn cleanup_expired_holds(&mut self) -> Result<(), String> {
        let current_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let expired_packages: Vec<String> = self.holds
            .iter()
            .filter(|(_, hold)| {
                hold.expires_at.map_or(false, |expires| expires < current_time)
            })
            .map(|(name, _)| name.clone())
            .collect();

        for package_name in expired_packages {
            self.unhold_package(&package_name)?;
        }

        Ok(())
    }

    fn is_hold_expired(&self, hold: &PackageHold) -> bool {
        if let Some(expires_at) = hold.expires_at {
            let current_time = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            current_time > expires_at
        } else {
            false
        }
    }

    fn save_holds(&self) -> Result<(), String> {
        let mut holds_path = get_metadata_dir()?;
        holds_path.push("holds.yaml");

        let mut file = File::create(&holds_path)
            .map_err(|_| "Failed to create holds file")?;

        let yaml = serde_norway::to_string(&self.holds)
            .map_err(|_| "Failed to serialize holds")?;

        file.write_all(yaml.as_bytes())
            .map_err(|_| "Failed to write holds file")?;

        Ok(())
    }

    fn save_version_pins(&self) -> Result<(), String> {
        let mut pins_path = get_metadata_dir()?;
        pins_path.push("version_pins.yaml");

        let mut file = File::create(&pins_path)
            .map_err(|_| "Failed to create version pins file")?;

        let yaml = serde_norway::to_string(&self.version_pins)
            .map_err(|_| "Failed to serialize version pins")?;

        file.write_all(yaml.as_bytes())
            .map_err(|_| "Failed to write version pins file")?;

        Ok(())
    }

    fn save_repository_pins(&self) -> Result<(), String> {
        let mut pins_path = get_metadata_dir()?;
        pins_path.push("repository_pins.yaml");

        let mut file = File::create(&pins_path)
            .map_err(|_| "Failed to create repository pins file")?;

        let yaml = serde_norway::to_string(&self.repository_pins)
            .map_err(|_| "Failed to serialize repository pins")?;

        file.write_all(yaml.as_bytes())
            .map_err(|_| "Failed to write repository pins file")?;

        Ok(())
    }

    pub fn load_holds(&mut self) -> Result<(), String> {
        let mut holds_path = get_metadata_dir()?;
        holds_path.push("holds.yaml");

        if holds_path.exists() {
            let mut file = File::open(&holds_path)
                .map_err(|_| "Failed to open holds file")?;

            let mut contents = String::new();
            file.read_to_string(&mut contents)
                .map_err(|_| "Failed to read holds file")?;

            self.holds = serde_norway::from_str(&contents)
                .map_err(|_| "Failed to parse holds file")?;
        }

        Ok(())
    }

    pub fn load_version_pins(&mut self) -> Result<(), String> {
        let mut pins_path = get_metadata_dir()?;
        pins_path.push("version_pins.yaml");

        if pins_path.exists() {
            let mut file = File::open(&pins_path)
                .map_err(|_| "Failed to open version pins file")?;

            let mut contents = String::new();
            file.read_to_string(&mut contents)
                .map_err(|_| "Failed to read version pins file")?;

            self.version_pins = serde_norway::from_str(&contents)
                .map_err(|_| "Failed to parse version pins file")?;
        }

        Ok(())
    }

    pub fn load_repository_pins(&mut self) -> Result<(), String> {
        let mut pins_path = get_metadata_dir()?;
        pins_path.push("repository_pins.yaml");

        if pins_path.exists() {
            let mut file = File::open(&pins_path)
                .map_err(|_| "Failed to open repository pins file")?;

            let mut contents = String::new();
            file.read_to_string(&mut contents)
                .map_err(|_| "Failed to read repository pins file")?;

            self.repository_pins = serde_norway::from_str(&contents)
                .map_err(|_| "Failed to parse repository pins file")?;
        }

        Ok(())
    }

    pub fn load_all(&mut self) -> Result<(), String> {
        self.load_holds()?;
        self.load_version_pins()?;
        self.load_repository_pins()?;
        Ok(())
    }
}

impl Default for PackageHoldManager {
    fn default() -> Self {
        Self::new()
    }
}

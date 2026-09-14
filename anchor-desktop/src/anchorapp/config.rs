/*
 * Main config file for handling all of the constnats / configuration
 */

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub static CONFIG_DIRECTORY_ROOT: OnceLock<PathBuf> = OnceLock::new();

pub static IDENTITY_DIRECTORY: OnceLock<PathBuf> = OnceLock::new();

pub fn init_config(config_dir: &Path) {
    let identity_dir = config_dir.join("identity");

    if !config_dir.exists() {
        fs::create_dir_all(&identity_dir).expect("Failed to create identity directory");
    }

    CONFIG_DIRECTORY_ROOT.set(config_dir.to_path_buf()).expect("Failed to set config directory");

    if !identity_dir.exists() {
        fs::create_dir_all(&identity_dir).expect("Failed to create identity directory");
    }

    IDENTITY_DIRECTORY.set(identity_dir.to_path_buf()).expect("Failed to set identity directory");
}

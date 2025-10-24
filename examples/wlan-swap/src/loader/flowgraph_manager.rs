//! Flowgraph Manager
//! 
//! Utilities for managing flowgraph lifecycle, including listing available
//! flowgraphs and reading/writing the control file.

use anyhow::Result;
use std::fs;
use std::path::Path;

pub const CONTROL_FILE: &str = ".flowgraph_control";

/// List all available flowgraph TOML files in the flowgraphs directory
pub fn list_flowgraphs() -> Result<Vec<String>> {
    let flowgraph_dir = Path::new("flowgraphs");
    let mut flowgraphs = Vec::new();
    
    if flowgraph_dir.exists() {
        for entry in fs::read_dir(flowgraph_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("toml") {
                if let Some(filename) = path.to_str() {
                    flowgraphs.push(filename.to_string());
                }
            }
        }
    }
    
    flowgraphs.sort();
    Ok(flowgraphs)
}

/// Read the current flowgraph from the control file
pub fn read_control_file() -> Result<String> {
    Ok(fs::read_to_string(CONTROL_FILE)?.trim().to_string())
}

/// Write a flowgraph path to the control file to trigger a reload
pub fn write_control_file(flowgraph_path: &str) -> Result<()> {
    fs::write(CONTROL_FILE, flowgraph_path)?;
    Ok(())
}

/// Check if the control file exists
pub fn control_file_exists() -> bool {
    Path::new(CONTROL_FILE).exists()
}

/// Get the display name of a flowgraph (without path and extension)
pub fn get_flowgraph_name(path: &str) -> String {
    Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(path)
        .to_string()
}

/// Get the category/type of a flowgraph from its name
pub fn get_flowgraph_category(name: &str) -> &str {
    if name.contains("wifi") {
        "WiFi"
    } else if name.contains("zigbee") {
        "ZigBee"
    } else if name.contains("loopback") {
        "Loopback"
    } else {
        "Other"
    }
}

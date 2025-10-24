//! TOML-based Flowgraph Loader
//! 
//! This module provides functionality to load and instantiate FutureSDR flowgraphs
//! from TOML configuration files.

use anyhow::{Result, Context};
use futuresdr::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use super::block_registry::BlockRegistry;

/// TOML Flowgraph Configuration
#[derive(Debug, Deserialize, Serialize)]
pub struct FlowgraphConfig {
    /// List of blocks in the flowgraph
    pub blocks: Vec<BlockConfig>,
    /// Stream connections between blocks
    #[serde(default)]
    pub connections: Vec<ConnectionConfig>,
    /// Message connections between blocks
    #[serde(default)]
    pub message_connections: Vec<MessageConnectionConfig>,
    /// Runtime configuration
    #[serde(default)]
    pub runtime: Option<RuntimeConfig>,
    /// CLI argument definitions
    #[serde(default)]
    pub cli: Option<CliConfig>,
}

/// Block configuration
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct BlockConfig {
    /// Unique name for this block instance
    pub name: String,
    /// Block type (e.g., "Apply", "Fft", "wifi::Decoder")
    #[serde(rename = "type")]
    pub block_type: String,
    /// Data type for typed blocks (e.g., "Complex32", "f32")
    #[serde(default)]
    pub dtype: Option<String>,
    /// Output type for Apply/Combine blocks
    #[serde(default)]
    pub output_type: Option<String>,
    /// Additional input types for Combine blocks
    #[serde(default)]
    pub input1_type: Option<String>,
    #[serde(default)]
    pub input2_type: Option<String>,
    /// Block parameters
    #[serde(default)]
    pub parameters: Vec<ParameterConfig>,
    /// Whether this block is optional (for conditional instantiation)
    #[serde(default)]
    pub optional: bool,
}

/// Block parameter configuration
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ParameterConfig {
    /// Parameter name
    pub name: String,
    /// Parameter type
    #[serde(rename = "type")]
    pub param_type: String,
    /// Parameter value (as string, will be parsed based on type)
    pub value: toml::Value,
}

/// Stream connection configuration
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ConnectionConfig {
    /// Source block name
    pub from: String,
    /// Source port (optional, defaults to "output" or first output)
    #[serde(default)]
    pub from_port: Option<String>,
    /// Destination block name
    pub to: String,
    /// Destination port (optional, defaults to "input" or first input)
    #[serde(default)]
    pub to_port: Option<String>,
    /// Conditional expression for this connection
    #[serde(default)]
    pub conditional: Option<String>,
}

/// Message connection configuration
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct MessageConnectionConfig {
    /// Source block name
    pub from: String,
    /// Source message port
    pub from_port: String,
    /// Destination block name
    pub to: String,
    /// Destination message port (optional)
    #[serde(default)]
    pub to_port: Option<String>,
    /// Conditional expression for this connection
    #[serde(default)]
    pub conditional: Option<String>,
}

/// Runtime configuration
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct RuntimeConfig {
    /// Async tasks to spawn
    #[serde(default)]
    pub async_tasks: Vec<AsyncTaskConfig>,
}

/// Async task configuration
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct AsyncTaskConfig {
    /// Block to interact with
    pub block: String,
    /// Port to send messages to
    pub port: String,
    /// Task type (e.g., "periodic_sender")
    pub task: String,
    /// Interval in seconds (for periodic tasks)
    #[serde(default)]
    pub interval_secs: Option<f32>,
    /// Message format (e.g., "Blob", "Any")
    pub message_format: String,
    /// Message pattern (template string)
    pub message_pattern: String,
    /// Extra parameters
    #[serde(default)]
    pub extra_params: Vec<ParameterConfig>,
}

/// CLI configuration
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct CliConfig {
    /// CLI argument definitions
    pub args: Vec<CliArgConfig>,
}

/// CLI argument configuration
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct CliArgConfig {
    /// Argument name
    pub name: String,
    /// Argument type
    #[serde(rename = "type")]
    pub arg_type: String,
    /// Default value
    #[serde(default)]
    pub default: Option<toml::Value>,
    /// Whether argument is optional
    #[serde(default)]
    pub optional: bool,
    /// Custom parser function name
    #[serde(default)]
    pub parser: Option<String>,
    /// Description
    #[serde(default)]
    pub description: Option<String>,
}

/// Flowgraph loader
pub struct FlowgraphLoader {
    config: FlowgraphConfig,
    block_map: HashMap<String, BlockId>,
    conditions: HashMap<String, bool>,
    registry: BlockRegistry,
}

impl FlowgraphLoader {
    /// Load flowgraph configuration from TOML file
    pub fn from_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        let content = std::fs::read_to_string(path.as_ref())
            .with_context(|| format!("Failed to read TOML file: {:?}", path.as_ref()))?;
        Self::from_str(&content)
    }

    /// Load flowgraph configuration from TOML string
    pub fn from_str(toml_str: &str) -> Result<Self> {
        let config: FlowgraphConfig = toml::from_str(toml_str)
            .context("Failed to parse TOML configuration")?;
        
        Ok(Self {
            config,
            block_map: HashMap::new(),
            conditions: HashMap::new(),
            registry: BlockRegistry::new(),
        })
    }

    /// Set a condition value (for conditional blocks/connections)
    pub fn set_condition(&mut self, name: String, value: bool) {
        self.conditions.insert(name, value);
    }

    /// Evaluate a conditional expression
    fn eval_condition(&self, expr: &Option<String>) -> bool {
        match expr {
            None => true,
            Some(e) => {
                // Simple evaluation: "name" or "!name"
                if let Some(name) = e.strip_prefix('!') {
                    !self.conditions.get(name).copied().unwrap_or(false)
                } else {
                    self.conditions.get(e).copied().unwrap_or(false)
                }
            }
        }
    }

    /// Build the flowgraph (placeholder - needs actual block creation logic)
    pub fn build(&mut self, fg: &mut Flowgraph) -> Result<()> {
        // Step 1: Create blocks
        for block_cfg in &self.config.blocks {
            if block_cfg.optional && !self.eval_condition(&Some(block_cfg.name.clone())) {
                continue;
            }

            let block_id = self.create_block(fg, block_cfg)?;
            self.block_map.insert(block_cfg.name.clone(), block_id);
        }

        // Step 2: Create stream connections
        for conn in &self.config.connections {
            if !self.eval_condition(&conn.conditional) {
                continue;
            }

            let from_id = self.block_map.get(&conn.from)
                .with_context(|| format!("Source block '{}' not found", conn.from))?;
            let to_id = self.block_map.get(&conn.to)
                .with_context(|| format!("Destination block '{}' not found", conn.to))?;

            let from_port = conn.from_port.as_deref().unwrap_or("output");
            let to_port = conn.to_port.as_deref().unwrap_or("input");

            fg.connect_dyn(*from_id, from_port, *to_id, to_port)?;
        }

        // Step 3: Create message connections
        for msg_conn in &self.config.message_connections {
            if !self.eval_condition(&msg_conn.conditional) {
                continue;
            }

            let from_id = self.block_map.get(&msg_conn.from)
                .with_context(|| format!("Source block '{}' not found", msg_conn.from))?;
            let to_id = self.block_map.get(&msg_conn.to)
                .with_context(|| format!("Destination block '{}' not found", msg_conn.to))?;

            println!("DEBUG: Connecting message: {} ({:?}) port '{}' -> {} ({:?})", 
                msg_conn.from, from_id, msg_conn.from_port, msg_conn.to, to_id);

            fg.connect_message(*from_id, msg_conn.from_port.as_str(), *to_id, 
                msg_conn.to_port.as_deref().unwrap_or(msg_conn.from_port.as_str()))?;
        }

        Ok(())
    }

    /// Create a block from configuration
    fn create_block(&self, fg: &mut Flowgraph, block_cfg: &BlockConfig) -> Result<BlockId> {
        self.registry.create_block(fg, block_cfg)
    }

    /// Get block ID by name
    pub fn get_block(&self, name: &str) -> Option<BlockId> {
        self.block_map.get(name).copied()
    }

    /// Get the configuration
    pub fn config(&self) -> &FlowgraphConfig {
        &self.config
    }
}

/// Convenience function to load a flowgraph from a TOML file
/// 
/// This is a high-level helper that creates a loader, builds the flowgraph,
/// and returns it ready to run.
pub fn load_flowgraph<P: AsRef<Path>>(path: P) -> Result<Flowgraph> {
    let mut loader = FlowgraphLoader::from_file(path)?;
    let mut fg = Flowgraph::new();
    loader.build(&mut fg)?;
    Ok(fg)
}

/// Load a flowgraph and return both the flowgraph and the loader
/// 
/// This allows access to block IDs and runtime configuration after loading.
pub fn load_flowgraph_with_loader<P: AsRef<Path>>(path: P) -> Result<(Flowgraph, FlowgraphLoader)> {
    let mut loader = FlowgraphLoader::from_file(path)?;
    let mut fg = Flowgraph::new();
    loader.build(&mut fg)?;
    Ok((fg, loader))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_simple_config() {
        let toml = r#"
[[blocks]]
name = "src"
type = "OneSource"
dtype = "u8"

[[blocks]]
name = "snk"
type = "FileSink"
dtype = "u8"
[[blocks.parameters]]
name = "path"
type = "string"
value = "out.dat"

[[connections]]
from = "src"
to = "snk"
        "#;

        let loader = FlowgraphLoader::from_str(toml).unwrap();
        assert_eq!(loader.config.blocks.len(), 2);
        assert_eq!(loader.config.connections.len(), 1);
    }

    #[test]
    fn test_conditional_evaluation() {
        let mut loader = FlowgraphLoader::from_str("[[blocks]]\nname = \"test\"\ntype = \"Test\"").unwrap();
        
        loader.set_condition("feature_a".to_string(), true);
        assert!(loader.eval_condition(&Some("feature_a".to_string())));
        assert!(!loader.eval_condition(&Some("!feature_a".to_string())));
        assert!(!loader.eval_condition(&Some("feature_b".to_string())));
        assert!(loader.eval_condition(&Some("!feature_b".to_string())));
    }
}

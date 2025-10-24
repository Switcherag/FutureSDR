//! Flowgraph loader module
//! 
//! Provides TOML-based flowgraph loading with block registry and management utilities

pub mod toml_loader;
pub mod block_registry;
pub mod flowgraph_manager;
pub mod flowgraph_controller;

pub use toml_loader::{FlowgraphLoader, load_flowgraph, load_flowgraph_with_loader};
pub use block_registry::BlockRegistry;
pub use flowgraph_manager::{
    list_flowgraphs, 
    read_control_file, 
    write_control_file, 
    control_file_exists,
    get_flowgraph_name,
    get_flowgraph_category,
    CONTROL_FILE
};
pub use flowgraph_controller::FlowgraphController;

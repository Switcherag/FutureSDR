use plugin_api::{FlowgraphController, default_plugin_dir};
use std::env;

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    futuresdr::runtime::init();

    let arg = env::args().nth(1).unwrap_or_default();

    match arg.as_str() {
        "--add-one" => {
            println!("Loading flowgraph: flows/fg_with_add_one.toml");
            FlowgraphController::builder(default_plugin_dir())
                .add_permanent("flows/fg_with_add_one.toml")
                .run()
        }
        "--hot-load" => {
            // Hot-load mode: permanent FG runs immediately, swappable slot
            // waits for a UDP swap command (e.g. -s flows/fg_with_add_one.toml).
            // The add_one plugin does NOT need to exist at startup.
            println!("Hot-load mode: permanent=fg_without_add_one, swappable=fg_idle");
            println!("Send UDP swap command to load a new flowgraph at runtime.");
            FlowgraphController::builder(default_plugin_dir())
                .add_permanent("flows/fg_without_add_one.toml")
                .add_swappable("flows/fg_idle.toml")
                .run()
        }
        _ => {
            println!("Loading flowgraph: flows/fg_without_add_one.toml");
            FlowgraphController::builder(default_plugin_dir())
                .add_permanent("flows/fg_without_add_one.toml")
                .run()
        }
    }
}

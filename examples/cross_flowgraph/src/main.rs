use plugin_api::{FlowgraphController, default_plugin_dir};

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    futuresdr::runtime::init();
    FlowgraphController::builder(default_plugin_dir())
        .add_permanent("flows/fg0.toml")        // fg/0/
        .add_swappable("flows/flow_a.toml")     // fg/1/
        .connect(0, "out", 1, "in")
        .udp_port(7878)
        .run()
}

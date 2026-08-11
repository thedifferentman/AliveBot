mod commands;
mod events;
mod tools;

use nagisa::prelude::*;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    App::new()
        .run_onebot(OneBotConfig::new("ws://127.0.0.1:8080"), ctrl_c_shutdown())
        .await
}

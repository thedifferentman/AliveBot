mod commands;
mod events;
mod utilities;

use nagisa::prelude::*;

#[tokio::main]
async fn main() -> Result<()> {
    App::new()
        .run_onebot(
            OneBotConfig::new("ws://127.0.0.1:8080"),
            ctrl_c_shutdown(),
        )
        .await
}

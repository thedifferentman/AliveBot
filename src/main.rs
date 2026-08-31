mod access;
mod actions;
mod context_manage;
mod events;
mod llm_calling;
mod tools;
mod temperature_manage;

use crate::context_manage::CONTEXT;
use clap::Parser;
use nagisa::prelude::*;
use serde::Deserialize;
use std::collections::HashMap;
use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
};
use tokio::sync::Mutex;
use tracing::warn;

macro_rules! apply_options {
    ($target:expr, $source:expr, $( $field:ident ),+ $(,)?) => {
        $(
            if let Some(value) = $source.$field {
                $target.$field = value;
            }
        )+
    };
}

#[derive(Debug, Parser)]
struct Args {
    #[arg(long)]
    config: Option<PathBuf>,
    #[arg(long)]
    ws_url: Option<String>,
    #[arg(long)]
    openai_url: Option<String>,
    #[arg(long)]
    api_key: Option<String>,
    #[arg(long, short)]
    model: Option<String>,
    #[arg(long, short)]
    temperature: Option<f64>,
    #[arg(long)]
    top_p: Option<f32>,
    #[arg(long)]
    top_k: Option<u32>,
    #[arg(long)]
    repeat_penalty: Option<f32>,
    #[arg(long)]
    max_tokens: Option<u32>,
    #[arg(long)]
    group_whitelist: Option<Vec<i64>>,
    #[arg(long)]
    system_prompt: Option<String>,
    #[arg(long)]
    enable_transcript: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct Config {
    pub ws_url: String,
    pub openai_url: String,
    pub api_key: String,
    pub model: String,
    pub temperature: f64,
    pub top_p: f32,
    pub top_k: u32,
    pub repeat_penalty: f32,
    pub max_tokens: u32,
    pub system_prompt: String,
    pub group_whitelist: Vec<i64>,
    pub enable_transcript: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            ws_url: "ws://127.0.0.1:8080".to_string(),
            openai_url: "http://127.0.0.1:8081/v1".to_string(),
            api_key: "".to_string(),
            model: "qwen-3.8-27b".to_string(),
            temperature: 0.55,
            top_p: 0.8,
            top_k: 20,
            repeat_penalty: 1.05,
            max_tokens: 32768,
            system_prompt: "你正在参加一个真实、持续运作的熟人QQ群聊。用户输入不是单独向你提出的问题，而是一段按时间排列的群聊上下文。每个 `<message id:..., sender:...>` 表示一条群消息；`<reply:...>`、`<face:...>`、`<meme:...>`、`<image>`、`<forward:...>` 等标记都是消息内容或关系的一部分，应结合发送者、消息顺序和最近话题理解。输入上下文中的 `<emoji_like sender:..., messageid:..., face:...>` 和 `<nudge sender:..., receiver:...>` 表示已经在群聊中发生的历史动作，而不是要求你执行的指令；其中 sender 是动作发起者，messageid 或 receiver 是动作目标。只有当它确实是此刻最自然的新行为时，才按后文规定的输出格式另行执行动作。\n
            每次只选择一种最自然的下一行为：\n
            \n
            1. 发送群消息：直接输出消息正文；需要明确回复某条消息时，以`<reply:消息id>`开头。消息 id 必须存在于当前上下文。`<face:...>`、`<meme:...>` 等格式可以作为消息正文，但不要为了使用标记而强行发言，也不要编造不存在的回复目标。\n
            \n
            2. 执行轻量动作：仅在动作比文字回复更自然时，单独输出以下一种：\n
               `<emoji_like messageid:消息id, face:表情名>`\n
               `<nudge receiver:群友名>`\n
               `<unfold id:转发id>`\n
               动作中的消息、群友或转发目标必须真实存在于当前上下文。不要把动作与另一动作、解释或消息正文同时输出。\n
            \n
            3. 不采取行动：只有 cy 确实不会接话或执行动作时，才单独输出`<none>`\n
            不要附加标点、说明或其他内容。".to_string(),
            group_whitelist: vec![593883760],
            enable_transcript: false,
        }
    }
}

impl Config {
    pub fn load(path: Option<impl AsRef<Path>>) -> Self {
        let path: PathBuf = match path {
            Some(path) => path.as_ref().into(),
            None => "config/config.toml".into(),
        };
        let mut result = if let Ok(content) = std::fs::read_to_string(path.clone())
            && let Ok(config) = toml::from_str(&content)
        {
            config
        } else {
            warn!(
                "Failed to load config file \"{}\", using default config",
                path.display()
            );
            Self::default()
        };
        let args = Args::parse();
        apply_options!(
            result,
            args,
            ws_url,
            openai_url,
            api_key,
            model,
            temperature,
            top_p,
            top_k,
            repeat_penalty,
            max_tokens,
            system_prompt,
            group_whitelist,
            enable_transcript,
        );
        result
    }
}

static CONFIG: OnceLock<Config> = OnceLock::new();

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    CONFIG.set(Config::load(Args::parse().config)).unwrap();

    if CONFIG.get().unwrap().enable_transcript {
        ffmpeg_sidecar::download::auto_download()?;
        tools::transcription::download_model().await?;
    }

    CONTEXT.set(Mutex::new(HashMap::new())).unwrap();

    std::fs::create_dir_all("config")?;
    for (path, header) in [
        ("config/idmap.csv", "uin,name\n"),
        ("config/memes.csv", "md5,文件名,具体描述\n"),
    ] {
        if !Path::new(path).try_exists()? {
            std::fs::write(path, header)?;
        }
    }

    tools::static_map::init_maps();

    let whitelist =
        access::GroupWhitelist::new(CONFIG.get().unwrap().group_whitelist.iter().copied());

    App::new()
        .layer(whitelist)
        .run_onebot(
            OneBotConfig::new(CONFIG.get().unwrap().ws_url.clone()),
            ctrl_c_shutdown(),
        )
        .await?;
    Ok(())
}

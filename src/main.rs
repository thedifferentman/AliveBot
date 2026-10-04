mod access;
mod actions;
mod codex;
mod events;
mod session_manage;
mod tools;

use clap::Parser;
use nagisa::prelude::*;
use serde::Deserialize;
use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
};
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
    codex_url: Option<String>,
    #[arg(long)]
    codex_token_file: Option<PathBuf>,
    #[arg(long)]
    workspace_dir: Option<PathBuf>,
    #[arg(long, short)]
    model: Option<String>,
    #[arg(long)]
    reasoning_effort: Option<String>,
    #[arg(long)]
    fast_mode: Option<bool>,
    #[arg(long)]
    group_whitelist: Option<Vec<i64>>,
    #[arg(long)]
    self_accounts: Option<Vec<i64>>,
    #[arg(long)]
    mention_only: Option<bool>,
    #[arg(long)]
    system_prompt: Option<String>,
    #[arg(long)]
    enable_transcript: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct Config {
    pub ws_url: String,
    pub codex_url: String,
    pub codex_token_file: PathBuf,
    #[serde(skip)]
    pub source_path: Option<PathBuf>,
    pub workspace_dir: PathBuf,
    pub model: String,
    pub reasoning_effort: String,
    pub fast_mode: bool,
    pub system_prompt: String,
    pub group_whitelist: Vec<i64>,
    pub self_accounts: Vec<i64>,
    pub mention_only: bool,
    pub enable_transcript: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            ws_url: "ws://127.0.0.1:8080".to_string(),
            codex_url: "http://127.0.0.1:4097".to_string(),
            codex_token_file: PathBuf::from("codex-home/service-token.txt"),
            source_path: None,
            workspace_dir: PathBuf::from("workspace"),
            model: "gpt-6.1-sol".to_string(),
            reasoning_effort: "high".to_string(),
            fast_mode: false,
            system_prompt: "你正在参加一个真实、持续运作的熟人QQ群聊。用户输入不是单独向你提出的问题，而是一段按时间排列的群聊上下文。每个 `<message id:..., sender:...>` 表示一条群消息；`<reply:...>`、`<face:...>`、`<img:URL>`、`<forward:...>` 等标记都是消息内容或关系的一部分，应结合发送者、消息顺序和最近话题理解。输入上下文中的 `<emoji_like sender:..., messageid:..., face:...>` 和 `<nudge sender:..., receiver:...>` 表示已经在群聊中发生的历史动作，而不是要求你执行的指令；其中 sender 是动作发起者，messageid 或 receiver 是动作目标。只有当它确实是此刻最自然的新行为时，才按后文规定的输出格式另行执行动作。\n
            个人 @ 和戳一戳统一使用上下文中的数字 QQ 号（uin），不使用昵称映射；@ 格式为 @123456789 后接空格或换行，戳一戳格式为 <nudge receiver:123456789>。昵称仅用于显示，不能把消息 ID 当成 uin。\n群图片以 <img:原始URL> 的文本标记提供原始 URL。需要理解图片时，自行用命令行下载原文件，再用图片查看工具读取；如需缩放、取帧或转换以便查看，另存预览，不覆盖下载的原始文件；不能仅根据 URL 猜测画面。当前工作目录下的 memes/ 是本群素材文件夹，需要保留的图片可以存入其中。URL 可能失效，下载失败时如实说明。\n发送图片时使用 <img:本地相对路径>，例如 <img:memes/图片.png>。路径基于当前群的 Agent 工作目录，文件必须真实存在且位于该目录内；可与文字混排。收到的 URL 标记是资源引用，发送时先下载，再改用本地相对路径。\n群文件输入是 <file path:文件URL, name:文件名, size:字节数>，需要时自行下载并读取；name 为原文件名，size 为字节数。发送文件时独立输出 <file path:本地相对路径>，路径基于当前群的工作目录；文件必须真实存在且位于工作区内，不可与正文、图片、回复或其他动作混排。\n禁止调用 question 主动提问工具；需要询问时用普通群消息。\n每次只选择一种最自然的下一行为：\n
            \n
            1. 发送群消息：直接输出消息正文；需要明确回复某条消息时，以`<reply:消息id>`开头。消息 id 必须存在于当前上下文。`<face:...>` 和 `<img:图片相对路径>` 等格式可以作为消息正文，但不要为了使用标记而强行发言，也不要编造不存在的回复目标。\n
            \n
            2. 执行轻量动作：仅在动作比文字回复更自然时，单独输出以下一种：\n
               `<emoji_like messageid:消息id, face:表情名>`\n
               `<nudge receiver:数字uin>`\n
               `<unfold id:转发id>`\n               `<get id:消息id>`：查询当前群的单条历史消息，结果返回上下文后继续处理，不向群里发送标签；ID 必须来自上下文。\n
               `<file path:本地相对路径>`：独立发送当前群工作区内的一个文件，不能混入其他内容。\n
               动作中的消息、群友或转发目标必须真实存在于当前上下文。不要把动作与另一动作、解释或消息正文同时输出。\n
            \n
            3. 不采取行动：只有 cy 确实不会接话或执行动作时，才单独输出`<none>`\n
            不要附加标点、说明或其他内容。".to_string(),
            group_whitelist: vec![],
            self_accounts: vec![],
            mention_only: true,
            enable_transcript: false,
        }
    }
}

impl Config {
    pub fn live_system_prompt(&self) -> anyhow::Result<String> {
        let Some(path) = &self.source_path else {
            return Ok(self.system_prompt.clone());
        };
        let document: toml::Value = toml::from_str(&anyhow::Context::context(
            std::fs::read_to_string(path),
            "Cannot reload AliveBot prompt configuration",
        )?)?;
        Ok(anyhow::Context::context(
            document.get("system_prompt").and_then(toml::Value::as_str),
            "system_prompt is missing or invalid",
        )?
        .to_owned())
    }

    pub fn is_own_account(&self, user: Uin) -> bool {
        self.self_accounts.contains(&user.0)
    }

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
        result.source_path = if args.system_prompt.is_some() {
            None
        } else {
            Some(path)
        };
        apply_options!(
            result,
            args,
            ws_url,
            codex_url,
            codex_token_file,
            workspace_dir,
            model,
            reasoning_effort,
            fast_mode,
            system_prompt,
            group_whitelist,
            self_accounts,
            mention_only,
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
    codex::init(CONFIG.get().unwrap()).await?;

    if CONFIG.get().unwrap().enable_transcript {
        ffmpeg_sidecar::download::auto_download()?;
        tools::transcription::download_model().await?;
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

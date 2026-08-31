pub mod static_map {
    use bimap::BiHashMap;
    use std::path::Path;
    use std::sync::OnceLock;

    pub struct StaticBiMap(OnceLock<BiHashMap<String, String>>);

    impl StaticBiMap {
        pub fn new(path: impl AsRef<Path>) -> BiHashMap<String, String> {
            let mut map = BiHashMap::new();
            let mut reader = csv::Reader::from_path(path.as_ref()).expect(
                format!(
                    "failed to open csv file {}",
                    path.as_ref().to_str().unwrap()
                )
                .as_str(),
            );
            for row in reader.records() {
                let row = row.unwrap();
                let Some(left) = row.get(0) else { continue };
                let Some(right) = row.get(1) else { continue };
                map.insert(left.to_string(), right.to_string());
            }
            map
        }

        pub fn get_by_left(&'static self, left: &str) -> Option<&'static str> {
            self.0
                .get()
                .expect("map not initialized")
                .get_by_left(left)
                .map(String::as_str)
        }

        pub fn get_by_right(&'static self, right: &str) -> Option<&'static str> {
            self.0
                .get()
                .expect("map not initialized")
                .get_by_right(right)
                .map(String::as_str)
        }
    }

    pub static ID_MAP: StaticBiMap = StaticBiMap(OnceLock::new());
    pub static FACE_MAP: StaticBiMap = StaticBiMap(OnceLock::new());
    pub static MEME_MAP: StaticBiMap = StaticBiMap(OnceLock::new());

    pub fn init_maps() {
        ID_MAP.0.set(StaticBiMap::new("config/idmap.csv")).unwrap();
        FACE_MAP
            .0
            .set(StaticBiMap::new("config/faces.csv"))
            .unwrap();
        MEME_MAP
            .0
            .set(StaticBiMap::new("config/memes.csv"))
            .unwrap();
    }
}

pub mod utility {
    use crate::tools::static_map::{ID_MAP, MEME_MAP};
    use nagisa::prelude::*;
    use std::path::PathBuf;

    const MEME_DIR: &str = "memes";

    pub(super) fn bad_params<T>(message: impl Into<String>) -> Result<T> {
        Err(Error::action_kind(ActionErrorKind::BadParams, message))
    }

    pub(super) fn user_uin(name: &str) -> Result<Uin> {
        let Some(uin) = ID_MAP.get_by_right(name.trim()) else {
            return bad_params(format!("unknown user: {name}"));
        };
        uin.parse::<i64>()
            .map(Uin)
            .map_err(|_| Error::action_kind(ActionErrorKind::BadParams, "invalid mapped uin"))
    }

    pub(super) fn meme_path(name: &str) -> Result<PathBuf> {
        if MEME_MAP.get_by_right(name).is_none() {
            return bad_params(format!("unknown meme: {name}"));
        }
        ["jpg", "png", "gif", "jpeg"]
            .into_iter()
            .map(|extension| PathBuf::from(MEME_DIR).join(format!("{name}.{extension}")))
            .find(|path| path.is_file())
            .ok_or_else(|| {
                Error::action_kind(
                    ActionErrorKind::NotFound,
                    format!("meme image not found: {name}"),
                )
            })
    }
}

pub mod message {
    use crate::CONFIG;
    use crate::tools::static_map::{FACE_MAP, ID_MAP};
    use crate::tools::transcription::transcribe_from_url;
    use crate::tools::utility::{bad_params, meme_path, user_uin};
    use nagisa::prelude::*;
    use serde_json::{Value, json};
    use tracing::warn;

    pub enum LiteSegment {
        Text(String),
        Image(String),
    }

    pub enum Outgoing {
        Noop,
        Unfold(String),
        Nudge(Uin),
        Reaction { message_id: i32, face: String },
        Segments(Vec<Segment>),
    }

    pub fn parse_outgoing(message: &str, peer: Peer) -> Result<Outgoing> {
        let message = message.trim();
        let parse_message_id = |value: &str| -> Result<i32> {
            let message_id = value.trim().parse::<i32>().map_err(|_| {
                Error::action_kind(ActionErrorKind::BadParams, "invalid OneBot message id")
            })?;
            if message_id == 0 {
                bad_params("invalid OneBot message id")
            } else {
                Ok(message_id)
            }
        };

        if message == "<none>" {
            return Ok(Outgoing::Noop);
        }

        if let Some(id) = message
            .strip_prefix("<unfold id:")
            .and_then(|value| value.strip_suffix('>'))
        {
            if id.is_empty() {
                return bad_params("unfold id is empty");
            }
            return Ok(Outgoing::Unfold(id.to_owned()));
        }

        if let Some(receiver) = message
            .strip_prefix("<nudge receiver:")
            .and_then(|value| value.strip_suffix('>'))
        {
            return Ok(Outgoing::Nudge(user_uin(receiver)?));
        }

        if let Some(args) = message
            .strip_prefix("<emoji_like ")
            .and_then(|value| value.strip_suffix('>'))
        {
            let Some(args) = args.strip_prefix("messageid:") else {
                return bad_params("invalid emoji_like message");
            };
            let Some((message_id, face)) = args.split_once(", face:") else {
                return bad_params("invalid emoji_like message");
            };
            let (face, _) = super::emoji::reaction_id(face)?;
            return Ok(Outgoing::Reaction {
                message_id: parse_message_id(message_id)?,
                face,
            });
        }

        if message.is_empty()
            || ["<none", "<unfold", "<nudge", "<emoji_like"]
                .iter()
                .any(|tag| message.contains(tag))
        {
            return bad_params("invalid standalone message");
        }

        let mut segments = Vec::new();
        let mut content = message;
        let (first_line, rest) = message.split_once('\n').unwrap_or((message, ""));
        let first_line = first_line.trim_end_matches('\r');
        if first_line.starts_with("<reply:") {
            let Some(id) = first_line
                .strip_prefix("<reply:")
                .and_then(|value| value.strip_suffix('>'))
            else {
                return bad_params("invalid reply message");
            };
            segments.push(Segment::reply(MessageId {
                peer,
                seq: 0,
                onebot_id: Some(parse_message_id(id)?),
            }));
            content = rest;
        } else if message.contains("<reply:") {
            return bad_params("reply must be on the first line");
        }

        parse_content(content, &mut segments)?;
        if segments.is_empty() {
            return bad_params("message content is empty");
        }
        Ok(Outgoing::Segments(segments))
    }

    fn parse_content(mut content: &str, segments: &mut Vec<Segment>) -> Result<()> {
        while !content.is_empty() {
            let next = [
                content.find("<face:"),
                content.find("<meme:"),
                content.find('@'),
            ]
            .into_iter()
            .flatten()
            .min();
            let Some(next) = next else {
                segments.push(Segment::text(content));
                break;
            };

            if next > 0 {
                segments.push(Segment::text(&content[..next]));
            }
            content = &content[next..];

            if let Some(rest) = content.strip_prefix("<face:") {
                let Some(end) = rest.find('>') else {
                    return bad_params("unclosed face tag");
                };
                let name = &rest[..end];
                let Some(id) = FACE_MAP.get_by_right(name) else {
                    return bad_params(format!("unknown face: {name}"));
                };
                segments.push(Segment::face(id));
                content = &rest[end + 1..];
            } else if let Some(rest) = content.strip_prefix("<meme:") {
                let Some(end) = rest.find('>') else {
                    return bad_params("unclosed meme tag");
                };
                let name = &rest[..end];
                segments.push(Segment::image_path(meme_path(name)?));
                content = &rest[end + 1..];
            } else {
                let rest = &content['@'.len_utf8()..];
                let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
                let name = &rest[..end];
                if name == "全体成员" {
                    segments.push(Segment::at_all());
                    content = &rest[end..];
                } else if ID_MAP.get_by_right(name).is_some() {
                    segments.push(Segment::at(user_uin(name)?));
                    content = &rest[end..];
                } else {
                    segments.push(Segment::text("@"));
                    content = rest;
                }
            }
        }
        Ok(())
    }

    pub async fn segments_to_lite(segments: Vec<Segment>) -> Vec<LiteSegment> {
        let mut result = Vec::<LiteSegment>::new();
        for segment in segments {
            use Segment::*;
            if let Some(segment) = match segment.clone() {
                Text(text) => Some(LiteSegment::Text(text)),

                Mention { user, .. } => ID_MAP
                    .get_by_left(user.0.to_string().as_str())
                    .map(|name| LiteSegment::Text(format!("@{}", name))),

                MentionAll => Some(LiteSegment::Text("@全体成员".to_string())),

                Face { id, .. } => FACE_MAP
                    .get_by_left(&id.to_string())
                    .map(|name| LiteSegment::Text(format!("<face:{}>", name))),

                Reply { id, .. } => id
                    .onebot_id
                    .map(|message_id| LiteSegment::Text(format!("<reply:{}>", message_id))),

                Image { res, .. } => res
                    .recv
                    .and_then(|res| res.url.map(|url| LiteSegment::Image(url))),

                Record { res, .. } => {
                    if !CONFIG.get().unwrap().enable_transcript {
                        None
                    } else {
                        async {
                            Some(LiteSegment::Text(format!(
                                "<record:{}>",
                                transcribe_from_url(res.recv.and_then(|recv| recv.url)?)
                                    .await
                                    .ok()?
                            )))
                        }
                        .await
                    }
                }

                Forward(forward) => match forward {
                    nagisa::prelude::Forward::Ref { id, .. } => {
                        Some(LiteSegment::Text(format!("<forward:{}>", id)))
                    }
                    _ => None,
                },

                _ => None,
            } {
                result.push(segment);
            } else {
                warn!("The broken segment \"{:?}\" has been skipped.", segment);
            }
        }
        result
    }

    impl Into<Value> for LiteSegment {
        fn into(self) -> Value {
            match self {
                LiteSegment::Text(text) => json!({
                    "type": "text",
                    "text": text
                }),
                LiteSegment::Image(url) => json!({
                    "type": "image_url",
                    "image_url": {
                        "url": url
                    }
                }),
            }
        }
    }
}

pub mod emoji {
    use crate::tools::static_map::FACE_MAP;
    use crate::tools::utility::bad_params;
    use nagisa::prelude::*;

    pub fn reaction_id(face: &str) -> Result<(String, ReactionKind)> {
        let face = face.trim();
        if let Some(id) = FACE_MAP.get_by_right(face) {
            return Ok((id.to_owned(), ReactionKind::Face));
        }
        if let Some(emoji) = emojito::find_emoji(face)
            .into_iter()
            .find(|emoji| emoji.glyph == face)
            && let Some(id) = napcat_emoji_id(emoji)
        {
            return Ok((id, ReactionKind::Emoji));
        }
        bad_params(format!("unknown reaction: {face}"))
    }

    pub fn reaction_name(id: &str, kind: ReactionKind) -> Option<String> {
        match kind {
            ReactionKind::Face => FACE_MAP.get_by_left(id).map(str::to_owned),
            ReactionKind::Emoji => id
                .parse::<u32>()
                .ok()
                .and_then(char::from_u32)
                .map(String::from),
        }
    }

    pub fn napcat_emoji_id(emoji: &emojito::Emoji) -> Option<String> {
        let mut codepoints = emoji
            .codepoint
            .split_ascii_whitespace()
            // FE0F 只是 Emoji 显示样式选择符
            .filter(|cp| !cp.eq_ignore_ascii_case("FE0F"));

        let first = codepoints.next()?;

        // 真正包含多个有效码点，不作为 QQ 回应发送
        if codepoints.next().is_some() {
            return None;
        }

        u32::from_str_radix(first, 16)
            .ok()
            .map(|value| value.to_string())
    }
}

pub mod transcription {
    use anyhow::{Context, Error, Result};
    use reqwest::Url;
    use std::path::Path;
    use tempfile::tempdir;
    use tokio::{fs::File, io::AsyncWriteExt, process::Command};
    use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

    const MODEL_PATH: &str = "models/ggml-small-q5_1.bin";
    const MODEL_URL: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small-q5_1.bin?download=true";

    pub async fn download_model() -> Result<()> {
        if Path::new(MODEL_PATH).is_file() {
            return Ok(());
        }
        tokio::fs::create_dir_all("models").await?;
        let model = reqwest::get(MODEL_URL)
            .await?
            .error_for_status()?
            .bytes()
            .await?;
        tokio::fs::write(MODEL_PATH, model).await?;
        Ok(())
    }

    async fn load_pcm(path: impl AsRef<Path>) -> Result<Vec<f32>> {
        let result = tokio::fs::read(path)
            .await
            .context("Failed to load pcm file.")?
            .chunks_exact(2)
            .map(|x| i16::from_le_bytes([x[0], x[1]]) as f32 / 32768.0)
            .collect();
        Ok(result)
    }

    pub async fn transcribe(path: impl AsRef<Path>) -> Result<String> {
        let pcm = load_pcm(path).await?;

        //加载模型
        let ctx = tokio::task::spawn_blocking(|| {
            WhisperContext::new_with_params(MODEL_PATH, WhisperContextParameters::default())
                .unwrap()
        })
        .await?;
        let mut params = FullParams::new(SamplingStrategy::BeamSearch {
            beam_size: 5,
            patience: -1.0,
        });
        params.set_language(None);

        //转录
        let mut state = ctx.create_state().context("Failed to create state.")?;
        let state = tokio::task::spawn_blocking(move || {
            state.full(params, pcm.as_slice()).unwrap();
            state
        })
        .await
        .context("Failed to run model.")?;

        //合并段落
        let mut result = String::new();
        for segment in state.as_iter() {
            result.push_str(
                &segment
                    .to_str()
                    .context("Failed to convert segment to string.")?,
            );
            result.push(' ');
        }
        Ok(result)
    }

    pub async fn transcribe_from_url(url: impl AsRef<str>) -> Result<String> {
        //下载音频文件
        let url = Url::parse(url.as_ref())?;
        let temp_dir = tempdir().context("Failed to create temporary directory.")?;
        let input = temp_dir.path().join("input.audio");
        let output = temp_dir.path().join("output.pcm");
        let mut response = reqwest::get(url)
            .await
            .context("Failed to download audio file.")?
            .error_for_status()
            .context("Audio server returned an error status.")?;
        let mut file = File::create(&input)
            .await
            .context("Failed to create temporary input file.")?;
        while let Some(chunk) = response.chunk().await? {
            file.write_all(&chunk).await?;
        }

        //转换文件
        file.flush().await?;
        drop(file);
        let status = Command::new(ffmpeg_sidecar::paths::ffmpeg_path())
            .arg("-nostdin")
            .arg("-y")
            .arg("-i")
            .arg(&input)
            .args(["-ar", "16000"])
            .args(["-ac", "1"])
            .args(["-f", "s16le"])
            .args(["-acodec", "pcm_s16le"])
            .arg(&output)
            .status()
            .await
            .context("Failed to start FFmpeg.")?;

        //启动转录
        if !status.success() {
            Err(Error::msg("FFmpeg failed to convert the audio."))
        } else {
            Ok(transcribe(&output).await?)
        }
    }
}

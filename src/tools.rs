pub mod static_map {
    use bimap::BiHashMap;
    use std::sync::OnceLock;

    pub struct StaticBiMap(OnceLock<BiHashMap<String, String>>);

    impl StaticBiMap {
        pub const fn new() -> StaticBiMap {
            StaticBiMap(OnceLock::new())
        }

        pub fn init(&'static self, data: Vec<(String, String)>) {
            let mut temp = BiHashMap::new();
            for (u, v) in data {
                temp.insert(u, v);
            }
            self.0.set(temp).expect("map already initialized");
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

    pub static ID_MAP: StaticBiMap = StaticBiMap::new();
    pub static FACE_MAP: StaticBiMap = StaticBiMap::new();
    pub static MEME_MAP: StaticBiMap = StaticBiMap::new();
}

pub mod message {
    use crate::tools::static_map::{FACE_MAP, ID_MAP};
    use crate::tools::transcript::transcribe_from_url;
    use nagisa::prelude::*;
    use tracing::{error, info, warn};

    pub enum LiteSegment {
        Text(String),
        Image(String),
    }

    pub async fn segments_to_lite(segments: Vec<Segment>) -> Vec<LiteSegment> {
        let mut result = Vec::<LiteSegment>::new();
        for segment in segments {
            use Segment::*;
            if let Some(segment) = match segment.clone() {
                Text(text) => Some(LiteSegment::Text(text)),

                Mention { user, .. } => ID_MAP
                    .get_by_left(user.0.to_string().as_str())
                    .map(|name| LiteSegment::Text(format!("<@{}>", name))),

                MentionAll => Some(LiteSegment::Text("@全体成员".to_string())),

                Face { id, .. } => FACE_MAP
                    .get_by_left(&id.to_string())
                    .map(|name| LiteSegment::Text(format!("<face:{}>", name))),

                Reply { id, .. } => id
                    .onebot_id
                    .map(|id| LiteSegment::Text(format!("<reply:{}>", id))),

                Image { res, .. } => res
                    .recv
                    .and_then(|res| res.url.map(|url| LiteSegment::Image(url))),

                Record { res, .. } => {
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
}

pub mod emoji {
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

pub mod transcript {
    use anyhow::{Context, Error, Result};
    use reqwest::Url;
    use std::path::Path;
    use tempfile::tempdir;
    use tokio::{fs::File, io::AsyncWriteExt, process::Command};
    use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

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
        static MODEL_PATH: &str = "./models/ggml-small-q5_1.bin";

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
        let status = Command::new("ffmpeg")
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

    #[tokio::test]
    async fn test_transcribe() {
        let result = transcribe(r"resources\test_audio.pcm").await.unwrap();
        println!("{}", result);
    }
}

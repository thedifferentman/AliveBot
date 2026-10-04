#[cfg(test)]
mod get_tests {
    use super::message::Outgoing;
    use nagisa::ImageSubType;
    use nagisa::prelude::*;

    fn parse_outgoing(message: &str, peer: Peer) -> Result<Outgoing> {
        super::message::parse_outgoing(message, peer, std::path::Path::new("."))
    }

    #[test]
    fn get_requires_one_standalone_nonzero_onebot_id() {
        let peer = Peer::group(Uin(123));
        for id in [1, -123, i32::MIN, i32::MAX] {
            assert!(
                matches!(parse_outgoing(&format!("<get id:{id}>"), peer), Ok(Outgoing::Get(actual)) if actual == id)
            );
        }
        for text in [
            "<get id:0>",
            "<get id:>",
            "<get id:abc>",
            "<get id:2147483648>",
            "<get id:1> extra",
            "text <get id:1>",
            "<get id:1>\n<get id:2>",
            "<reply:1>\n<get id:2>",
        ] {
            assert!(parse_outgoing(text, peer).is_err(), "accepted: {text}");
        }
    }

    #[test]
    fn retired_image_actions_are_rejected_instead_of_sent_as_text() {
        let peer = Peer::group(Uin(123));
        for text in [
            "<collect messageid:1>",
            "<collect messageid:-123>",
            "<meme:旧表情>",
            "正文 <meme:旧表情>",
            "<reply:1>\n<meme:旧表情>",
        ] {
            assert!(parse_outgoing(text, peer).is_err(), "accepted: {text}");
        }
    }

    #[test]
    fn numeric_mentions_and_nudges_do_not_need_user_maps() {
        let peer = Peer::group(Uin(123));
        let Ok(Outgoing::Segments(parts)) =
            parse_outgoing("@123456789 你好 @987654321\n正文", peer)
        else {
            panic!("expected message")
        };
        let targets: Vec<_> = parts
            .iter()
            .filter_map(|part| match part {
                Segment::Mention { user, .. } => Some(user.0),
                _ => None,
            })
            .collect();
        assert_eq!(targets, vec![123456789, 987654321]);
        assert!(matches!(
            parse_outgoing("<nudge receiver:987654321>", peer),
            Ok(Outgoing::Nudge(Uin(987654321)))
        ));
        for target in [
            "昵称",
            "",
            "0",
            "-123",
            "+123",
            "１２３",
            "9223372036854775808",
        ] {
            assert!(parse_outgoing(&format!("<nudge receiver:{target}>"), peer).is_err());
        }
        for text in [
            "@昵称 你好",
            "@0",
            "@-123",
            "@123，正文",
            "contact@example.com",
        ] {
            let Ok(Outgoing::Segments(parts)) = parse_outgoing(text, peer) else {
                panic!("expected plain text")
            };
            assert!(
                !parts
                    .iter()
                    .any(|part| matches!(part, Segment::Mention { .. }))
            );
            assert_eq!(
                parts
                    .iter()
                    .filter_map(Segment::as_text)
                    .collect::<String>(),
                text
            );
        }
        let Ok(Outgoing::Segments(parts)) = parse_outgoing("@全体成员 提醒", peer) else {
            panic!("expected message")
        };
        assert!(matches!(parts[0], Segment::MentionAll));
    }

    #[tokio::test]
    async fn incoming_mention_keeps_uin_and_separator() {
        use super::message::{LiteSegment, segments_to_lite};
        let parts =
            segments_to_lite(vec![Segment::at(Uin(123456789)), Segment::text("正文")]).await;
        let text: String = parts
            .into_iter()
            .filter_map(|part| match part {
                LiteSegment::Text(text) => Some(text),
                _ => None,
            })
            .collect();
        assert_eq!(text, "@123456789 正文");
    }

    #[test]
    fn local_images_mix_with_text_and_resolve_from_the_group_workspace() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("group");
        std::fs::create_dir_all(directory.join("memes")).unwrap();
        std::fs::write(directory.join("memes/image with spaces.png"), b"fixture").unwrap();
        let peer = Peer::group(Uin(123));
        let message = "<reply:12>\n看看这个 <img:memes/image with spaces.png> @123456789 后文";
        let Ok(Outgoing::Segments(parts)) =
            super::message::parse_outgoing(message, peer, &directory)
        else {
            panic!("expected image message")
        };
        assert!(matches!(parts[0], Segment::Reply { .. }));
        let path = parts
            .iter()
            .find_map(|part| match part {
                Segment::Image {
                    res,
                    sub_type: ImageSubType::Normal,
                    ..
                } => match &res.source {
                    Some(ResourceSource::Path(path)) => Some(path),
                    _ => None,
                },
                _ => None,
            })
            .unwrap();
        assert!(path.is_absolute());
        assert_eq!(
            path.canonicalize().unwrap(),
            directory
                .join("memes/image with spaces.png")
                .canonicalize()
                .unwrap()
        );
        assert!(parts.iter().any(|part| matches!(
            part,
            Segment::Mention {
                user: Uin(123456789),
                ..
            }
        )));
        assert_eq!(
            parts
                .iter()
                .filter_map(Segment::as_text)
                .collect::<String>(),
            "看看这个   后文"
        );
    }

    #[test]
    fn outgoing_images_require_existing_files_inside_the_workspace() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("group");
        std::fs::create_dir_all(directory.join("memes")).unwrap();
        std::fs::write(temp.path().join("outside.png"), b"outside").unwrap();
        std::fs::write(directory.join("memes/inside.png"), b"inside").unwrap();
        let peer = Peer::group(Uin(123));
        for value in [
            "",
            "memes/missing.png",
            "memes",
            "../outside.png",
            "https://example.invalid/image.png",
            "C:\\outside.png",
        ] {
            assert!(
                super::message::parse_outgoing(&format!("<img:{value}>"), peer, &directory)
                    .is_err(),
                "accepted: {value}"
            );
        }
        assert!(
            super::message::parse_outgoing(
                &format!("<img:{}>", directory.join("memes/inside.png").display()),
                peer,
                &directory
            )
            .is_err()
        );
        assert!(super::message::parse_outgoing("<img:memes/inside.png", peer, &directory).is_err());
        assert!(
            super::message::parse_outgoing(
                "<img:memes/inside.png><img:./memes/inside.png>",
                peer,
                &directory
            )
            .is_ok()
        );
    }

    #[test]
    fn file_upload_is_one_standalone_action_with_a_workspace_path() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("files")).unwrap();
        let file = temp.path().join("files/报告 1.zip");
        std::fs::write(&file, b"fixture").unwrap();
        let peer = Peer::group(Uin(123));
        let Ok(Outgoing::File(path)) =
            super::message::parse_outgoing("<file path:files/报告 1.zip>", peer, temp.path())
        else {
            panic!("expected file action")
        };
        assert_eq!(path.canonicalize().unwrap(), file.canonicalize().unwrap());
        for text in [
            "正文 <file path:files/报告 1.zip>",
            "<file path:files/报告 1.zip> 正文",
            "<file path:files/报告 1.zip>\n<file path:files/报告 1.zip>",
            "<reply:12>\n<file path:files/报告 1.zip>",
            "<file path:files/报告 1.zip><img:files/报告 1.zip>",
            "<file path:>",
            "<file path:https://example.invalid/a.zip>",
            "<file path:../outside.zip>",
            "<file path:files>",
            "<file path:missing.zip>",
            "<file path:files/报告 1.zip",
        ] {
            assert!(
                super::message::parse_outgoing(text, peer, temp.path()).is_err(),
                "accepted: {text}"
            );
        }
    }
}

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

    pub static FACE_MAP: StaticBiMap = StaticBiMap(OnceLock::new());

    pub fn init_maps() {
        FACE_MAP
            .0
            .set(StaticBiMap::new("config/faces.csv"))
            .unwrap();
    }
}

pub mod utility {
    use nagisa::prelude::*;
    use std::path::{Component, Path, PathBuf};
    pub(super) fn bad_params<T>(message: impl Into<String>) -> Result<T> {
        Err(Error::action_kind(ActionErrorKind::BadParams, message))
    }

    pub(super) fn user_uin(value: &str) -> Result<Uin> {
        let value = value.trim();
        if value.is_empty() || !value.bytes().all(|c| c.is_ascii_digit()) {
            return bad_params("uin must contain only decimal digits");
        }
        match value.parse::<i64>() {
            Ok(uin) if uin > 0 => Ok(Uin(uin)),
            _ => bad_params("uin must be a positive i64 integer"),
        }
    }

    pub(super) fn image_path(directory: &Path, value: &str) -> Result<PathBuf> {
        workspace_file_path(directory, value, "img")
    }

    pub(super) fn workspace_file_path(
        directory: &Path,
        value: &str,
        kind: &str,
    ) -> Result<PathBuf> {
        let value = value.trim();
        let relative = Path::new(value);
        if value.is_empty()
            || value
                .chars()
                .any(|c| c.is_control() || matches!(c, ':' | '<' | '>'))
            || relative.is_absolute()
            || relative
                .components()
                .any(|part| matches!(part, Component::Prefix(_) | Component::RootDir))
        {
            return bad_params(format!(
                "{kind} requires a local path relative to this group's workspace"
            ));
        }
        let root = directory
            .canonicalize()
            .map_err(|error| Error::action(error.to_string()))?;
        let path = root.join(relative).canonicalize().map_err(|_| {
            Error::action_kind(
                ActionErrorKind::NotFound,
                format!("{kind} file unavailable: {value}"),
            )
        })?;
        if !path.starts_with(&root) {
            return bad_params(format!(
                "{kind} path must stay inside this group's workspace"
            ));
        }
        if !path.is_file() {
            return Err(Error::action_kind(
                ActionErrorKind::NotFound,
                format!("{kind} path is not a file"),
            ));
        }
        // NapCat needs a normal Windows path rather than a verbatim path.
        let text = path.to_string_lossy();
        Ok(if let Some(tail) = text.strip_prefix(r"\\?\UNC\") {
            PathBuf::from(format!(r"\\{tail}"))
        } else {
            PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(&text))
        })
    }
}

pub mod message {
    use crate::CONFIG;
    use crate::tools::static_map::FACE_MAP;
    use crate::tools::transcription::transcribe_from_url;
    use crate::tools::utility::{bad_params, image_path, user_uin, workspace_file_path};
    use nagisa::prelude::*;
    use std::path::{Path, PathBuf};
    use tracing::warn;

    pub enum LiteSegment {
        Text(String),
        Image(String),
        File {
            name: String,
            size: u64,
            url: Option<String>,
        },
    }

    pub enum Outgoing {
        Noop,
        Unfold(String),
        Get(i32),
        File(PathBuf),
        Nudge(Uin),
        Reaction { message_id: i32, face: String },
        Segments(Vec<Segment>),
    }

    pub fn parse_outgoing(message: &str, peer: Peer, directory: &Path) -> Result<Outgoing> {
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

        if let Some(path) = message
            .strip_prefix("<file path:")
            .and_then(|value| value.strip_suffix('>'))
        {
            return Ok(Outgoing::File(workspace_file_path(
                directory, path, "file",
            )?));
        }

        if let Some(id) = message
            .strip_prefix("<get id:")
            .and_then(|value| value.strip_suffix('>'))
        {
            return Ok(Outgoing::Get(parse_message_id(id)?));
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
            || [
                "<none",
                "<unfold",
                "<get",
                "<file",
                "<collect",
                "<meme",
                "<nudge",
                "<emoji_like",
            ]
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

        parse_content(content, &mut segments, directory)?;
        if segments.is_empty() {
            return bad_params("message content is empty");
        }
        Ok(Outgoing::Segments(segments))
    }

    fn parse_content(
        mut content: &str,
        segments: &mut Vec<Segment>,
        directory: &Path,
    ) -> Result<()> {
        while !content.is_empty() {
            let next = [
                content.find("<face:"),
                content.find("<img:"),
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
            } else if let Some(rest) = content.strip_prefix("<img:") {
                let Some(end) = rest.find('>') else {
                    return bad_params("unclosed img tag");
                };
                segments.push(Segment::image_path(image_path(directory, &rest[..end])?));
                content = &rest[end + 1..];
            } else {
                let rest = &content['@'.len_utf8()..];
                let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
                let name = &rest[..end];
                if name == "全体成员" {
                    segments.push(Segment::at_all());
                    content = &rest[end..];
                } else if let Ok(uin) = user_uin(name) {
                    segments.push(Segment::at(uin));
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

                Mention { user, .. } => Some(LiteSegment::Text(format!("@{} ", user.0))),

                MentionAll => Some(LiteSegment::Text("@全体成员".to_string())),

                Face { id, .. } => FACE_MAP
                    .get_by_left(&id.to_string())
                    .map(|name| LiteSegment::Text(format!("<face:{}>", name))),

                Reply { id, .. } => id
                    .onebot_id
                    .map(|message_id| LiteSegment::Text(format!("<reply:{}>", message_id))),

                Image { res, .. } => Some(match res.recv.and_then(|res| res.url) {
                    Some(url) => LiteSegment::Image(url),
                    None => LiteSegment::Text("<img:unavailable>".into()),
                }),

                File {
                    name, size, url, ..
                } => Some(LiteSegment::File { name, size, url }),

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

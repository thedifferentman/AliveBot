use crate::tools::{
    emoji::reaction_name,
    message::{LiteSegment, segments_to_lite},
};
use crate::{CONFIG, codex::Prompt, session_manage};
use nagisa::*;
use std::collections::VecDeque;
use tracing::warn;

fn message_header(id: Option<&str>, sender: &str, uin: &str) -> String {
    let mut fields = Vec::new();
    if let Some(id) = id {
        fields.push(format!("id:{id}"));
    }
    fields.push(format!("sender:{sender}"));
    fields.push(format!("uin:{uin}"));
    format!("<message {}>\n", fields.join(", "))
}

fn rule_not_command(ctx: &Ctx) -> bool {
    let Some(message) = ctx.message() else {
        return true;
    };
    let mut content = VecDeque::from(message.content.clone());
    while !content.is_empty() {
        if let Some(Segment::Text(text)) = content.front() {
            return !text.trim_start().starts_with('/');
        }
        content.pop_front();
    }
    true
}

#[event(Ready)]
async fn ready(bot: Bot) -> HandlerResult {
    session_manage::restore(bot)
        .await
        .map_err(|e| Error::action(e.to_string()))
}

#[event(Message, gate = Rule::pred(rule_not_command))]
async fn common_message(bot: Bot, message: MessageEvent) -> HandlerResult {
    let own = CONFIG.get().unwrap().is_own_account(message.sender);
    let should_reply = !CONFIG.get().unwrap().mention_only
        || message
            .content
            .iter()
            .any(|segment| matches!(segment, Segment::Mention {user,..} if *user == bot.self_id()));
    let message_id = message.id.onebot_id;
    let file_id = match message.content.as_slice() {
        [Segment::File { id, .. }] if !id.is_empty() => Some(id.clone()),
        _ => None,
    };
    if message_id.is_none()
        && !message
            .content
            .iter()
            .any(|part| matches!(part, Segment::File { .. }))
    {
        warn!("message without OneBot message_id skipped");
        return Ok(());
    }
    let group = session_manage::get(bot.clone(), message.peer)
        .await
        .map_err(|e| Error::action(e.to_string()))?;
    let _ingress = group.ingress.lock().await;
    if let Some(id) = message_id {
        if group.is_model_echo(id, message.sender).await {
            return Ok(());
        }
    }
    if message_id.is_none() {
        if let Some(id) = &file_id {
            if group.is_model_file_echo(id, message.sender).await {
                return Ok(());
            }
        }
    }
    let event_key = message_id
        .map(|id| format!("message:{id}"))
        .or_else(|| file_id.map(|id| format!("file:{}:{id}", message.sender.0)));
    let prompt = message_prompt(&bot, message, message_id).await;
    group
        .enqueue(prompt, should_reply, own, event_key)
        .await
        .map_err(|e| Error::action(e.to_string()))
}

#[event(GroupFileUpload)]
async fn group_file_message(bot: Bot, notice: Notice) -> HandlerResult {
    let Notice::GroupFileUpload { group, user, file } = notice else {
        return Ok(());
    };
    let own = CONFIG.get().unwrap().is_own_account(user);
    let peer = Peer::group(group);
    let session = session_manage::get(bot.clone(), peer)
        .await
        .map_err(|e| Error::action(e.to_string()))?;
    let _ingress = session.ingress.lock().await;
    if session.is_model_file_echo(&file.id, user).await {
        return Ok(());
    }
    let event_key = (!file.id.is_empty()).then(|| format!("file:{}:{}", user.0, file.id));
    let mut prompt = Prompt::text(message_header(None, &user.to_string(), &user.to_string()));
    let parts = resolve_file_urls(
        &bot,
        peer,
        vec![Segment::File {
            id: file.id,
            name: file.name,
            size: file.size,
            hash: file.hash,
            url: None,
        }],
    )
    .await;
    prompt.append(lite_prompt(segments_to_lite(parts).await).await);
    prompt.text.push_str("\n</message>");
    session
        .enqueue(prompt, !CONFIG.get().unwrap().mention_only, own, event_key)
        .await
        .map_err(|e| Error::action(e.to_string()))
}

async fn message_prompt(bot: &Bot, message: MessageEvent, message_id: Option<i32>) -> Prompt {
    let sender = message
        .member
        .as_ref()
        .map(|m| m.display_name().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| message.sender.to_string());
    let mut prompt = Prompt::text(message_header(
        message_id.map(|id| id.to_string()).as_deref(),
        &sender,
        &message.sender.to_string(),
    ));
    let parts = resolve_file_urls(bot, message.peer, message.content).await;
    prompt.append(lite_prompt(segments_to_lite(parts).await).await);
    prompt.text.push_str("\n</message>");
    prompt
}

pub async fn get_message_prompt(bot: Bot, peer: Peer, id: i32) -> Result<Prompt> {
    let message = bot
        .get_message(&MessageId {
            peer,
            seq: 0,
            onebot_id: Some(id),
        })
        .await?;
    // OneBot get_msg addresses messages globally. Never expose another group's
    // or a private conversation's content to this group.
    if message.peer != peer || message.id.onebot_id != Some(id) {
        return Err(Error::action(
            "queried message does not match the requested group and id",
        ));
    }
    let mut prompt = Prompt::text(format!(
        "<get id:{id}>\n以下是查询得到的历史消息，不是新发言。\n"
    ));
    prompt.append(message_prompt(&bot, message, Some(id)).await);
    prompt.text.push_str("\n</get>");
    Ok(prompt)
}

pub async fn lite_prompt(segments: Vec<LiteSegment>) -> Prompt {
    let mut prompt = Prompt::default();
    for segment in segments {
        match segment {
            LiteSegment::Text(text) => prompt.text.push_str(&text),
            LiteSegment::Image(uri) => {
                // Keep expiring QQ URLs intact, including their query parameters.
                // The agent downloads and views them; this layer never fetches them.
                if resource_url(&uri) {
                    prompt.text.push_str(&format!("<img:{uri}>"));
                } else {
                    warn!("QQ image has no usable HTTP(S) URL");
                    prompt.text.push_str("<img:unavailable>");
                }
            }
            LiteSegment::File { name, size, url } => {
                let name = file_tag_name(&name);
                if let Some(url) = url.filter(|url| resource_url(url)) {
                    prompt
                        .text
                        .push_str(&format!("\n<file path:{url}, name:{name}, size:{size}>\n"));
                } else {
                    prompt.text.push_str(&format!("\n<file path:unavailable, name:{name}, size:{size}>\n文件下载链接不可用，不要把 unavailable 当成本地路径。\n"));
                }
            }
        }
    }
    prompt
}

fn file_tag_name(name: &str) -> String {
    name.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\r', "&#13;")
        .replace('\n', "&#10;")
}

fn resource_url(uri: &str) -> bool {
    !uri.chars()
        .any(|c| c.is_control() || matches!(c, '<' | '>'))
        && reqwest::Url::parse(uri)
            .is_ok_and(|url| matches!(url.scheme(), "http" | "https") && url.has_host())
}

async fn resolve_file_urls(bot: &Bot, peer: Peer, mut parts: Vec<Segment>) -> Vec<Segment> {
    for part in &mut parts {
        if let Segment::File { id, url, .. } = part {
            if url.as_deref().is_some_and(resource_url) {
                continue;
            }
            *url = None;
            if peer.is_group() && !id.is_empty() {
                match bot.get_group_file_download_url(peer.id, id).await {
                    Ok(link) if resource_url(&link) => *url = Some(link),
                    _ => warn!(group = peer.id.0, "Group file download URL unavailable"),
                }
            }
        }
    }
    parts
}

pub async fn forward_prompt(bot: Bot, peer: Peer, id: &str) -> Result<Prompt> {
    let nodes = bot.get_forward_messages(id).await?;
    if nodes.is_empty() {
        warn!(forward_id = id, "Forward lookup returned no nodes");
        return Err(Error::action("forward lookup returned no nodes"));
    }
    let mut prompt = Prompt::text(format!("<forward id:{id}>\n"));
    for node in nodes {
        let sender = if node.name.is_empty() {
            node.user.to_string()
        } else {
            node.name
        };
        prompt
            .text
            .push_str(&message_header(None, &sender, &node.user.0.to_string()));
        let parts = resolve_file_urls(&bot, peer, node.content).await;
        let content = lite_prompt(segments_to_lite(parts).await).await;
        if content.text.trim().is_empty() && content.files.is_empty() {
            prompt
                .text
                .push_str("[该条转发消息没有可读取的内容，不能推断原消息为空]");
        } else {
            prompt.append(content);
        }
        prompt.text.push_str("\n</message>\n");
    }
    prompt.text.push_str("</forward>");
    Ok(prompt)
}

#[cfg(test)]
mod identity_tests {
    use super::message_header;

    #[test]
    fn renamed_sender_keeps_stable_uin() {
        let first = message_header(Some("message-1"), "旧昵称", "123456789");
        let next = message_header(Some("message-2"), "新昵称", "123456789");
        assert!(first.contains("uin:123456789"));
        assert!(next.contains("uin:123456789"));
        assert!(next.contains("sender:新昵称"));
    }

    #[test]
    fn forwarded_message_retains_source_uin_without_inventing_message_id() {
        let header = message_header(None, "转发作者", "987654321");
        assert!(header.contains("uin:987654321"));
        assert!(!header.contains("id:"));
        assert!(!header.contains("own:"));
    }
}

#[cfg(test)]
mod image_url_tests {
    use super::*;
    use nagisa::prelude::{MilkyActions, OneBotActions};

    #[tokio::test]
    async fn original_image_urls_are_text_only_and_need_no_network_fetch() {
        let url = "http://127.0.0.1:9/original.gif?file=raw&rkey=a%2Bb%26c&scene=1";
        let prompt = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            lite_prompt(vec![
                LiteSegment::Text("前文".into()),
                LiteSegment::Image(url.into()),
                LiteSegment::Text("后文".into()),
            ]),
        )
        .await
        .unwrap();
        assert_eq!(prompt.text, format!("前文<img:{url}>后文"));
        assert!(prompt.files.is_empty());
        assert!(
            serde_json::to_value(&prompt)
                .unwrap()
                .get("files")
                .is_none()
        );
        assert!(!prompt.text.contains("data:image/"));
    }

    #[tokio::test]
    async fn unusable_urls_and_missing_image_references_remain_visible() {
        for url in [
            "",
            "not a URL",
            "data:image/png;base64,AAAA",
            "file:///C:/image.png",
            "https://example.invalid/image\n<control>injected</control>",
        ] {
            let prompt = lite_prompt(vec![LiteSegment::Image(url.into())]).await;
            assert_eq!(prompt.text, "<img:unavailable>");
            assert!(prompt.files.is_empty());
        }
        let mut image = Segment::image_url("unused");
        if let Segment::Image { res, .. } = &mut image {
            res.recv = None;
        }
        let prompt = lite_prompt(segments_to_lite(vec![image]).await).await;
        assert_eq!(prompt.text, "<img:unavailable>");
    }

    struct ForwardImages;
    #[nagisa::async_trait]
    impl nagisa::adapter::ActionInvoker for ForwardImages {
        fn protocol(&self) -> Protocol {
            Protocol::OneBot11
        }
        async fn call_raw(&self, _: &str, _: serde_json::Value) -> Result<serde_json::Value> {
            Ok(serde_json::Value::Null)
        }
        async fn send(&self, _: &Peer, _: &[Segment]) -> Result<MessageId> {
            Err(Error::action("forward image test must not send to QQ"))
        }
        async fn get_forward_messages(&self, _: &str) -> Result<Vec<ForwardNode>> {
            let mut nodes = Vec::new();
            for (index, url) in [
                "https://example.invalid/first.png?rkey=first",
                "https://example.invalid/second.gif?rkey=second",
            ]
            .iter()
            .enumerate()
            {
                let mut image = Segment::image_url("unused");
                if let Segment::Image { res, .. } = &mut image {
                    res.recv = Some(nagisa::ResourceRef {
                        id: None,
                        url: Some((*url).into()),
                        raw: serde_json::Value::Null,
                    });
                }
                nodes.push(ForwardNode {
                    user: Uin(100 + index as i64),
                    name: format!("作者{index}"),
                    content: vec![image],
                    time: None,
                });
            }
            Ok(nodes)
        }
    }
    impl OneBotActions for ForwardImages {}
    impl MilkyActions for ForwardImages {}

    #[tokio::test]
    async fn forwarded_images_keep_their_original_urls_and_authors() {
        let bot = Bot::new(std::sync::Arc::new(ForwardImages), Uin(1));
        let prompt = forward_prompt(bot, Peer::group(Uin(123)), "fixture")
            .await
            .unwrap();
        assert!(prompt.text.contains("uin:100"));
        assert!(prompt.text.contains("uin:101"));
        assert!(
            prompt
                .text
                .contains("https://example.invalid/first.png?rkey=first")
        );
        assert!(
            prompt
                .text
                .contains("https://example.invalid/second.gif?rkey=second")
        );
        assert_eq!(prompt.text.matches("<img:").count(), 2);
        assert!(prompt.files.is_empty());
    }
}

#[cfg(test)]
mod file_tests {
    use super::*;
    use nagisa::prelude::{MilkyActions, OneBotActions};
    use tokio::sync::Mutex;

    struct FileLookup {
        queries: std::sync::Arc<Mutex<Vec<(Uin, String)>>>,
        fail: bool,
    }
    #[nagisa::async_trait]
    impl nagisa::adapter::ActionInvoker for FileLookup {
        fn protocol(&self) -> Protocol {
            Protocol::OneBot11
        }
        async fn call_raw(&self, _: &str, _: serde_json::Value) -> Result<serde_json::Value> {
            Ok(serde_json::Value::Null)
        }
        async fn send(&self, _: &Peer, _: &[Segment]) -> Result<MessageId> {
            panic!("receiving a file must not send a QQ message")
        }
        async fn get_group_file_download_url(&self, group: Uin, id: &str) -> Result<String> {
            self.queries.lock().await.push((group, id.into()));
            if self.fail {
                Err(Error::action("file URL unavailable"))
            } else {
                Ok(format!(
                    "https://example.invalid/download?file={id}&key=a%2Bb%26c"
                ))
            }
        }
    }
    impl OneBotActions for FileLookup {}
    impl MilkyActions for FileLookup {}

    fn input_file(url: Option<&str>) -> Segment {
        Segment::File {
            id: "file-123".into(),
            name: "报告 1.zip".into(),
            size: 12345,
            hash: None,
            url: url.map(str::to_owned),
        }
    }

    #[tokio::test]
    async fn file_receive_resolves_a_group_url_and_keeps_metadata_without_attachments() {
        let queries = std::sync::Arc::new(Mutex::new(Vec::new()));
        let bot = Bot::new(
            std::sync::Arc::new(FileLookup {
                queries: queries.clone(),
                fail: false,
            }),
            Uin(1),
        );
        let peer = Peer::group(Uin(123));
        let parts = resolve_file_urls(&bot, peer, vec![input_file(None)]).await;
        let prompt = lite_prompt(segments_to_lite(parts).await).await;
        assert_eq!(*queries.lock().await, vec![(Uin(123), "file-123".into())]);
        assert!(prompt.text.lines().any(|line|line=="<file path:https://example.invalid/download?file=file-123&key=a%2Bb%26c, name:报告 1.zip, size:12345>"));
        assert!(prompt.text.contains("报告 1.zip"));
        assert!(prompt.text.contains("12345"));
        assert!(prompt.files.is_empty());
        assert!(!prompt.text.contains("<img:"));
        assert!(!prompt.text.contains("\"fileId\""));
        assert!(!prompt.text.contains('{'));
    }

    #[tokio::test]
    async fn provided_urls_are_reused_and_lookup_failures_are_explicit() {
        let queries = std::sync::Arc::new(Mutex::new(Vec::new()));
        let bot = Bot::new(
            std::sync::Arc::new(FileLookup {
                queries: queries.clone(),
                fail: true,
            }),
            Uin(1),
        );
        let peer = Peer::group(Uin(123));
        let parts = resolve_file_urls(
            &bot,
            peer,
            vec![input_file(Some(
                "https://example.invalid/original?token=keep",
            ))],
        )
        .await;
        let prompt = lite_prompt(segments_to_lite(parts).await).await;
        assert!(prompt.text.contains(
            "<file path:https://example.invalid/original?token=keep, name:报告 1.zip, size:12345>"
        ));
        assert!(queries.lock().await.is_empty());
        let parts = resolve_file_urls(&bot, peer, vec![input_file(None)]).await;
        let prompt = lite_prompt(segments_to_lite(parts).await).await;
        assert!(
            prompt
                .text
                .contains("<file path:unavailable, name:报告 1.zip, size:12345>")
        );
        assert!(prompt.files.is_empty());
    }

    #[tokio::test]
    async fn native_file_messages_without_message_ids_keep_the_uploader_header() {
        let queries = std::sync::Arc::new(Mutex::new(Vec::new()));
        let bot = Bot::new(
            std::sync::Arc::new(FileLookup {
                queries,
                fail: false,
            }),
            Uin(1),
        );
        let peer = Peer::group(Uin(123));
        let message = MessageEvent {
            id: MessageId {
                peer,
                seq: 0,
                onebot_id: None,
            },
            peer,
            sender: Uin(200),
            self_id: Uin(1),
            time: 0,
            content: vec![input_file(None)],
            is_self: false,
            group: None,
            member: None,
            friend: None,
            anonymous: None,
            font: None,
            target_id: None,
            message_style: None,
            raw: serde_json::Value::Null,
        };
        let prompt = message_prompt(&bot, message, None).await;
        assert!(prompt.text.starts_with("<message sender:200, uin:200>"));
        assert!(!prompt.text.contains("own:"));
        assert!(prompt.text.contains("<file path:https://"));
        assert!(!prompt.text.contains("<message id:"));
    }
}

#[event(Reaction, gate = Rule::pred(rule_not_command))]
async fn reaction_message(bot: Bot, notice: Notice) -> HandlerResult {
    let Notice::Reaction {
        group,
        user,
        seq,
        face_id,
        kind,
        is_add: true,
        ..
    } = notice
    else {
        return Ok(());
    };
    let own = CONFIG.get().unwrap().is_own_account(user);
    let Some(face) = reaction_name(&face_id, kind) else {
        return Ok(());
    };
    let Ok(message_id) = i32::try_from(seq) else {
        return Ok(());
    };
    if message_id == 0 {
        return Ok(());
    }
    let session = session_manage::get(bot.clone(), Peer::group(group))
        .await
        .map_err(|e| Error::action(e.to_string()))?;
    let _ingress = session.ingress.lock().await;
    let sender = user.0;
    session
        .enqueue(
            Prompt::text(format!(
                "<emoji_like sender:{sender}, messageid:{message_id}, face:{face}>"
            )),
            !CONFIG.get().unwrap().mention_only,
            own,
            None,
        )
        .await
        .map_err(|e| Error::action(e.to_string()))
}

#[event(Nudge, gate = Rule::pred(rule_not_command))]
async fn nudge_message(bot: Bot, notice: Notice) -> HandlerResult {
    let Notice::GroupNudge {
        group,
        sender,
        receiver,
        ..
    } = notice
    else {
        return Ok(());
    };
    let own = CONFIG.get().unwrap().is_own_account(sender);
    let session = session_manage::get(bot.clone(), Peer::group(group))
        .await
        .map_err(|e| Error::action(e.to_string()))?;
    let _ingress = session.ingress.lock().await;
    let sender = sender.0;
    let receiver = receiver.0;
    session
        .enqueue(
            Prompt::text(format!("<nudge sender:{sender}, receiver:{receiver}>")),
            !CONFIG.get().unwrap().mention_only,
            own,
            None,
        )
        .await
        .map_err(|e| Error::action(e.to_string()))
}

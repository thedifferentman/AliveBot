use crate::session_manage;
use crate::tools;
use crate::tools::message::Outgoing;
use nagisa::prelude::*;
use std::path::Path;
use tracing::{info, warn};

#[command("/ping")]
async fn ping(reply: Reply) -> HandlerResult {
    reply.text("pong").await?;
    Ok(())
}

#[command("/new")]
pub async fn new(bot: Bot, message_event: MessageEvent) -> HandlerResult {
    let group = session_manage::get(bot, message_event.peer)
        .await
        .map_err(|e| Error::action(e.to_string()))?;
    let _ingress = group.ingress.lock().await;
    group
        .reset()
        .await
        .map_err(|e| Error::action(e.to_string()))?;
    info!("New Codex session requested; workspace files preserved");
    Ok(())
}

#[command("/stop")]
async fn stop(bot: Bot, message_event: MessageEvent) -> HandlerResult {
    let group = session_manage::get(bot, message_event.peer)
        .await
        .map_err(|e| Error::action(e.to_string()))?;
    let _ingress = group.ingress.lock().await;
    group
        .stop()
        .await
        .map_err(|e| Error::action(e.to_string()))?;
    info!("Codex task stopped; session and workspace preserved");
    Ok(())
}

#[command("/face")]
async fn face(reply: Reply, CommandArg(segments): CommandArg) -> HandlerResult {
    let Some(segment) = segments.first() else {
        bail!(
            ActionErrorKind::BadParams,
            "Need 1 number param, find no param."
        );
    };
    let Some(text) = segment.as_text() else {
        bail!(
            ActionErrorKind::BadParams,
            "Need 1 number param, find other type."
        );
    };
    reply.face(text).await?;
    Ok(())
}

#[command("/faceid")]
async fn faceid(reply: Reply, CommandArg(segments): CommandArg) -> HandlerResult {
    let Some(segment) = segments.first() else {
        bail!(
            ActionErrorKind::BadParams,
            "Need 1 face param, find no param."
        );
    };
    let Segment::Face { id, .. } = segment else {
        bail!(
            ActionErrorKind::BadParams,
            "Need 1 face param, find other type."
        );
    };
    reply.text(id).await?;
    Ok(())
}

#[derive(Args)]
struct ReactArgs {
    #[arg(reply)]
    id: MessageId,

    #[arg(face, rest)]
    faces: Vec<String>,

    #[arg(rest)]
    content: String,
}

#[command("/react")]
async fn react(bot: Bot, Args(ReactArgs { id, faces, content }): Args<ReactArgs>) -> HandlerResult {
    for face in faces {
        bot.actions()
            .set_msg_reaction(&id, face.as_str(), true)
            .await?;
    }
    let emojis = emojito::find_emoji(content);
    for emoji in emojis {
        if let Some(emoji) = tools::emoji::napcat_emoji_id(emoji) {
            bot.actions()
                .set_msg_reaction(&id, emoji.as_str(), true)
                .await?;
        } else {
            warn!("Do not support hybrid emoji \"{}\".", emoji.glyph);
        }
    }
    Ok(())
}

pub enum ReadRequest {
    Unfold(String),
    Get(i32),
}

pub enum Delivery {
    NoMessage,
    Sent(MessageId),
    FileUploaded { id: String, name: String },
    Read(ReadRequest),
}

pub async fn send_message(
    bot: Bot,
    message: &str,
    peer: Peer,
    directory: &Path,
) -> Result<Delivery> {
    match tools::message::parse_outgoing(message, peer, directory)? {
        Outgoing::Noop => {
            info!(
                group = peer.id.0,
                reason = "model returned <none>",
                "Model reply skipped"
            );
        }
        Outgoing::Unfold(id) => return Ok(Delivery::Read(ReadRequest::Unfold(id))),
        Outgoing::Get(id) => return Ok(Delivery::Read(ReadRequest::Get(id))),
        Outgoing::File(path) => {
            if !peer.is_group() {
                return Err(Error::action_kind(
                    ActionErrorKind::BadParams,
                    "file upload requires a group conversation",
                ));
            }
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| Error::action("file has no usable filename"))?
                .to_owned();
            let id = bot
                .upload_group_file(peer.id, ResourceSource::Path(path), &name, None)
                .await?;
            return Ok(Delivery::FileUploaded { id, name });
        }
        Outgoing::Nudge(receiver) => {
            bot.send_nudge(&peer, receiver).await?;
        }
        Outgoing::Reaction { message_id, face } => {
            bot.actions()
                .set_msg_reaction(
                    &MessageId {
                        peer,
                        seq: 0,
                        onebot_id: Some(message_id),
                    },
                    &face,
                    true,
                )
                .await?;
        }
        Outgoing::Segments(segments) => {
            return Ok(Delivery::Sent(bot.send(&peer, &segments).await?));
        }
    }
    Ok(Delivery::NoMessage)
}

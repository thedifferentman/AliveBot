use crate::context_manage::{CONTEXT, SharedContext};
use crate::events::forward_message;
use crate::tools;
use crate::tools::message::Outgoing;
use nagisa::prelude::*;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use tracing::{info, warn};

#[command("/ping")]
async fn ping(reply: Reply) -> HandlerResult {
    reply.text("pong").await?;
    Ok(())
}

#[command("/new")]
pub async fn new(message_event: MessageEvent) -> HandlerResult {
    let mut contexts = CONTEXT.get().unwrap().lock().await;
    if let Some(previous) = contexts.get(&message_event.peer) {
        previous.revision.fetch_add(1, Ordering::Relaxed);
    }
    contexts.insert(message_event.peer.clone(), Arc::new(SharedContext::new()));
    info!("New context created");
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

pub async fn send_message(
    bot: Bot,
    message: &str,
    peer: Peer,
    shared_context: Arc<SharedContext>,
    expected_revision: u64,
) -> HandlerResult {
    let current_revision = shared_context.revision.load(Ordering::Relaxed);
    if current_revision != expected_revision {
        info!(
            expected_revision,
            current_revision, "Newer message arrived; outgoing action was cancelled"
        );
        return Ok(());
    }
    let skip_probability = {
        let context = shared_context.context.lock().await;
        context.temp_decay.skip_probability()
    };

    #[cfg(debug_assertions)]
    let _guard= tools::utility::CONFIRM_LOCK.lock().await;

    let current_revision = shared_context.revision.load(Ordering::Relaxed);
    if current_revision != expected_revision {
        info!(
            expected_revision,
            current_revision, "Newer message arrived; outgoing action was cancelled"
        );
        return Ok(());
    }
    if rand::random::<f64>() < skip_probability {
        info!("Message skipped.");
        return Ok(());
    }

    #[cfg(debug_assertions)]
    if !tools::utility::confirm(
        format!(
            "Are you sure to send message \"{}\" to group {}?",
            message, peer.id
        )
            .as_str(),
    )
        .await
        .unwrap()
    {
        return Ok(());
    }

    #[cfg(debug_assertions)]
    drop(_guard);

    let current_revision = shared_context.revision.load(Ordering::Relaxed);
    if current_revision != expected_revision {
        info!(
            expected_revision,
            current_revision, "Newer message arrived; outgoing action was cancelled"
        );
        return Ok(());
    }

    match tools::message::parse_outgoing(message, peer)? {
        Outgoing::Noop => {}
        Outgoing::Unfold(id) => {
            Box::pin(forward_message(
                bot,
                &id,
                peer,
                shared_context,
                expected_revision,
            ))
            .await?;
        }
        Outgoing::Nudge(receiver) => {
            bot.send_nudge(&peer, receiver).await?;
            shared_context.context.lock().await.temp_decay.increase();
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
            shared_context.context.lock().await.temp_decay.increase();
        }
        Outgoing::Segments(segments) => {
            bot.send(&peer, &segments).await?;
            shared_context.context.lock().await.temp_decay.increase();
        }
    }
    Ok(())
}

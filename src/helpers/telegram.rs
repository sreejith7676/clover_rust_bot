use teloxide::{
    payloads::SendMessage,
    prelude::*,
    requests::JsonRequest,
    types::{Message, MessageId, ParseMode, ReplyParameters, UserId},
};

/// Sender of a message, or `UserId(0)` for messages without one (channel posts, etc.).
pub fn sender_id(msg: &Message) -> UserId {
    msg.from.as_ref().map_or(UserId(0), |u| u.id)
}

/// HTML-formatted reply to `msg`. All dynamic content must already be escaped.
pub fn reply_html(bot: &Bot, msg: &Message, text: impl Into<String>) -> JsonRequest<SendMessage> {
    bot.send_message(msg.chat.id, text)
        .parse_mode(ParseMode::Html)
        .reply_parameters(ReplyParameters::new(msg.id))
}

/// Best-effort delete (placeholder messages etc.).
pub async fn delete_quiet(bot: &Bot, chat: ChatId, id: MessageId) {
    if let Err(e) = bot.delete_message(chat, id).await {
        log::debug!("Could not delete message {}: {}", id.0, e);
    }
}

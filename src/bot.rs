mod callback;
mod opml_commands;

use std::{fmt::Write, time::Duration};

use anyhow::{Context, Result};
use teloxide_core::{
    Bot as Telegram,
    payloads::GetUpdatesSetters,
    requests::{HasPayload, Requester},
    types::{
        AllowedUpdate, BotCommand, ChatFullInfo, ChatId, InlineKeyboardButton,
        InlineKeyboardMarkup, Me, Message, MessageEntity, MessageEntityKind, MessageId, ParseMode,
        ReplyParameters, Update, UpdateKind, UserId,
    },
};

use crate::{
    app::{AddSubscriptionError, AlreadySubscribed, App},
    telegram::{MARKDOWN, disable_link_preview},
};

const COMMANDS: &[(&str, &str)] = &[
    ("start", "开始使用"),
    ("ping", ""),
    ("sub", "订阅RSS源"),
    ("unsub", "退订RSS源"),
    ("list", "已订阅的RSS源"),
    ("unsuball", "取消所有订阅"),
    ("set", "设置订阅"),
    ("setfeedtag", "设置rss订阅标签"),
    ("setinterval", "设置订阅刷新频率"),
    ("export", "导出OPML"),
    ("import", "导入OPML文件"),
    ("pauseall", "停止抓取所有订阅更新"),
    ("activeall", "开启抓取订阅更新"),
    ("help", "帮助"),
    ("version", "Bot 版本信息"),
];

const HELP: &str = "\n\t命令：\n\t/sub 订阅源\n\t/unsub  取消订阅\n\t/list 查看当前订阅源\n\t/set 设置订阅\n\t/check 检查当前订阅\n\t/setfeedtag 设置订阅标签\n\t/setinterval 设置订阅刷新频率\n\t/activeall 开启所有订阅\n\t/pauseall 暂停所有订阅\n\t/help 帮助\n\t/import 导入 OPML 文件\n\t/export 导出 OPML 文件\n\t/unsuball 取消所有订阅\n\t详细使用方法请看：https://github.com/indes/flowerss-bot\n\t";

pub async fn run(app: &App, telegram: &Telegram, polling: &Telegram) -> Result<()> {
    let me = telegram
        .get_me()
        .await
        .context("getting Telegram bot identity")?;
    let bot = Bot { app, telegram, me };
    let commands = COMMANDS
        .iter()
        .filter(|(_, description)| !description.is_empty())
        .map(|(command, description)| BotCommand::new(*command, *description));
    telegram.set_my_commands(commands).await?;

    let mut offset = 1;
    loop {
        let updates = polling
            .get_updates()
            .offset(offset)
            .timeout(10)
            .allowed_updates([AllowedUpdate::Message, AllowedUpdate::CallbackQuery])
            .await;
        match updates {
            Ok(updates) => {
                for update in updates {
                    offset = offset
                        .max(i32::try_from(update.id.0.saturating_add(1)).unwrap_or(i32::MAX));
                    if let Err(error) = bot.update(update).await {
                        tracing::warn!(%error, "Telegram update handling failed");
                    }
                }
            }
            Err(error) => {
                tracing::warn!(%error, "Telegram polling failed");
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }
}

struct Bot<'a> {
    app: &'a App,
    telegram: &'a Telegram,
    me: Me,
}

struct CommandContext<'a> {
    message: &'a Message,
    payload: &'a str,
    mention_name: Option<&'a str>,
    mentioned: Option<ChatFullInfo>,
    bot_is_mentioned_admin: bool,
}

impl CommandContext<'_> {
    fn target(&self) -> i64 {
        self.mentioned
            .as_ref()
            .map_or(self.message.chat.id, |chat| chat.id)
            .0
    }

    fn arguments(&self) -> std::borrow::Cow<'_, str> {
        match self.mention_name {
            Some(mention) => self.payload.replace(mention, "").into(),
            None => self.payload.into(),
        }
    }
}

struct View {
    text: String,
    mode: Option<ParseMode>,
    keyboard: Option<InlineKeyboardMarkup>,
}

impl View {
    fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            mode: None,
            keyboard: None,
        }
    }

    fn markdown(text: impl Into<String>) -> Self {
        Self {
            mode: Some(MARKDOWN),
            ..Self::plain(text)
        }
    }

    fn html(text: impl Into<String>) -> Self {
        Self {
            mode: Some(ParseMode::Html),
            ..Self::plain(text)
        }
    }

    fn keyboard(self, keyboard: Vec<Vec<InlineKeyboardButton>>) -> Self {
        Self {
            keyboard: Some(InlineKeyboardMarkup::new(keyboard)),
            ..self
        }
    }
}

impl Bot<'_> {
    async fn send(&self, message: &Message, reply: bool, view: View) -> Result<()> {
        let reply_to = (reply && message.id.0 != 0).then_some(message.id);
        self.send_view(message.chat.id, reply_to, view).await
    }

    async fn send_view(&self, chat: ChatId, reply_to: Option<MessageId>, view: View) -> Result<()> {
        let mut request = self.telegram.send_message(chat, view.text);
        let payload = request.payload_mut();
        payload.parse_mode = view.mode;
        payload.reply_markup = view.keyboard.map(Into::into);
        payload.reply_parameters = reply_to.map(ReplyParameters::new);
        payload.link_preview_options = view.mode.map(|_| disable_link_preview());
        request.await?;
        Ok(())
    }

    async fn plain(&self, message: &Message, reply: bool, text: &str) -> Result<()> {
        self.send(message, reply, View::plain(text)).await
    }

    fn allowed(&self, user: UserId) -> bool {
        self.app.config.allowed_users.is_empty()
            || self
                .app
                .config
                .allowed_users
                .contains(&ChatId::from(user).0)
    }

    async fn is_admin(&self, chat: ChatId, private: bool, user: UserId) -> bool {
        if private {
            return true;
        }
        match self.telegram.get_chat_administrators(chat).await {
            Ok(admins) => admins.iter().any(|admin| admin.user.id == user),
            Err(error) => {
                tracing::warn!(chat = chat.0, %error, "Telegram administrator lookup failed");
                false
            }
        }
    }

    async fn bot_is_admin(&self, chat: ChatId) -> bool {
        self.telegram
            .get_chat_member(chat, self.me.id)
            .await
            .is_ok_and(|member| member.is_privileged())
    }

    async fn update(&self, update: Update) -> Result<()> {
        match update.kind {
            UpdateKind::Message(message) => self.message(&message).await,
            UpdateKind::CallbackQuery(callback) => self.callback(callback).await,
            _ => Ok(()),
        }
    }

    async fn message(&self, message: &Message) -> Result<()> {
        let text = message.text().unwrap_or_default();
        let command = parse_command(text, self.me.user.username.as_deref().unwrap_or_default());
        if command.is_none() && (message.document().is_none() || !text.is_empty()) {
            return Ok(());
        }
        let Some(sender) = &message.from else {
            return Ok(());
        };
        if !self.allowed(sender.id) {
            return Ok(());
        }

        let mention_name = mention(message);
        let mentioned = if let Some(name) = mention_name {
            match self.telegram.get_chat(name.to_owned()).await {
                Ok(chat) => Some(chat),
                Err(error) => {
                    tracing::warn!(%error, name, "Telegram mention lookup failed");
                    None
                }
            }
        } else {
            None
        };

        if !self
            .is_admin(message.chat.id, message.chat.is_private(), sender.id)
            .await
        {
            return self.plain(message, true, "您不是当前会话的管理员").await;
        }
        let mut bot_is_mentioned_admin = false;
        if let Some(chat) = &mentioned {
            let authorized = if chat.is_private() {
                true
            } else {
                match self.telegram.get_chat_administrators(chat.id).await {
                    Ok(admins) => {
                        bot_is_mentioned_admin =
                            admins.iter().any(|admin| admin.user.id == self.me.id);
                        admins.iter().any(|admin| admin.user.id == sender.id)
                    }
                    Err(error) => {
                        tracing::warn!(%error, chat = chat.id.0, "Telegram administrator lookup failed");
                        false
                    }
                }
            };
            if !authorized {
                return self.plain(message, true, "您不是当前会话的管理员").await;
            }
        }
        let (name, payload) = command.unwrap_or(("", ""));
        let ctx = CommandContext {
            message,
            payload,
            mention_name,
            mentioned,
            bot_is_mentioned_admin,
        };
        match name {
            "start" => self.plain(message, false, "你好，欢迎使用flowerss。").await,
            "ping" => self.plain(message, false, "pong").await,
            "help" => self.plain(message, false, HELP).await,
            "version" => self.send(message, false, View::plain(crate::version())).await,
            "sub" => self.subscribe(&ctx).await,
            "unsub" => self.unsubscribe(&ctx).await,
            "list" => self.list(&ctx).await,
            "unsuball" | "set" | "setfeedtag" | "setinterval" | "activeall" | "pauseall" | ""
                if ctx.mention_name.is_some() && ctx.mentioned.is_none() => {
                self.plain(message, true, "获取频道信息错误").await
            }
            "unsuball" => self.confirm_unsubscribe_all(&ctx).await,
            "set" => self.pick_setting(&ctx).await,
            "setfeedtag" => self.set_tag(&ctx).await,
            "setinterval" => self.set_interval(&ctx).await,
            "pauseall" | "activeall" => self.set_active(&ctx, name == "activeall").await,
            "import" => self.plain(message, true, "请直接发送OPML文件，如果需要为频道导入OPML，请在发送文件的时候附上channel id，例如@telegram").await,
            "export" => self.export_opml(&ctx).await,
            "" => self.import_opml(&ctx).await,
            _ => unreachable!("registered command missing handler"),
        }
    }

    async fn subscribe(&self, ctx: &CommandContext<'_>) -> Result<()> {
        let message = ctx.message;
        let Some(url) = find_url(ctx) else {
            return if ctx.mention_name.is_some() {
                self.plain(message, false, "频道订阅请使用' /sub @ChannelID URL ' 命令")
                    .await
            } else {
                self.plain(
                    message,
                    true,
                    "请在命令后带上需要订阅的RSS URL，例如：/sub https://justinpot.com/feed/",
                )
                .await
            };
        };
        let separator = if ctx.mention_name.is_some() {
            let Some(chat) = &ctx.mentioned else {
                return self.plain(message, true, "获取频道信息失败").await;
            };
            if !chat.is_channel()
                || (!ctx.bot_is_mentioned_admin && !self.bot_is_admin(chat.id).await)
            {
                return self
                    .plain(message, true, "您或Bot不是频道管理员，无法设置订阅")
                    .await;
            }
            " "
        } else {
            ""
        };
        match self.app.subscribe(ctx.target(), url).await {
            Ok(source) => {
                self.send(
                    message,
                    true,
                    View::markdown(format!(
                        "[[{}]]{separator}[{}]({}) 订阅成功",
                        source.id, source.title, source.link
                    )),
                )
                .await
            }
            Err(error) if error.is::<AlreadySubscribed>() => {
                self.plain(message, true, "已订阅该源，请勿重复订阅").await
            }
            Err(error) if error.is::<AddSubscriptionError>() => {
                tracing::warn!(%error, "saving subscription failed");
                self.plain(message, true, "订阅失败").await
            }
            Err(error) => {
                self.send(message, true, View::plain(format!("{error}，订阅失败")))
                    .await
            }
        }
    }

    async fn unsubscribe(&self, ctx: &CommandContext<'_>) -> Result<()> {
        let message = ctx.message;
        let Some(url) = find_url(ctx) else {
            return if ctx.mention_name.is_some() {
                self.plain(
                    message,
                    false,
                    "频道退订请使用' /unsub @ChannelID URL ' 命令",
                )
                .await
            } else {
                self.pick_unsubscribe(ctx).await
            };
        };
        if ctx.mention_name.is_some() && ctx.mentioned.is_none() {
            return self.plain(message, true, "获取频道信息错误").await;
        }
        let Ok(Some(source)) = self.app.store.find_source(url).await else {
            let error = if ctx.mentioned.is_some() {
                "获取订阅信息错误"
            } else {
                "未订阅该RSS源"
            };
            return self.plain(message, true, error).await;
        };
        if !matches!(
            self.app
                .store
                .remove_subscription(ctx.target(), source.id)
                .await,
            Ok(true)
        ) {
            return self.plain(message, true, "退订失败").await;
        }
        let text = if let Some(chat) = &ctx.mentioned {
            format!(
                "频道 [{}](https://t.me/{}) 退订 [{}]({}) 成功",
                chat.title().unwrap_or_default(),
                chat.username().unwrap_or_default(),
                source.title,
                source.link
            )
        } else {
            format!("[{}]({}) 退订成功！", source.title, source.link)
        };
        self.send(message, false, View::markdown(text)).await
    }

    async fn list(&self, ctx: &CommandContext<'_>) -> Result<()> {
        let message = ctx.message;
        if ctx.mention_name.is_some() && ctx.mentioned.is_none() {
            return self.plain(message, false, "获取频道信息错误").await;
        }
        let Ok(subscriptions) = self.app.store.subscriptions(ctx.target()).await else {
            return self.plain(message, false, "获取订阅错误").await;
        };
        if subscriptions.is_empty() {
            return self.plain(message, false, "订阅列表为空").await;
        }
        let mut text = format!("共订阅{}个源，订阅列表\n", subscriptions.len());
        for page in subscriptions.chunks(50) {
            for (source, _) in page {
                let _ = writeln!(
                    text,
                    "[[{}]] [{}]({})",
                    source.id, source.title, source.link
                );
            }
            self.send(message, false, View::markdown(std::mem::take(&mut text)))
                .await?;
        }
        Ok(())
    }

    async fn set_tag(&self, ctx: &CommandContext<'_>) -> Result<()> {
        let arguments = ctx.arguments();
        let mut args = arguments.trim().split(' ');
        let source_id = args.next().and_then(|id| id.parse().ok()).unwrap_or(0);
        let Ok(Some(mut subscription)) = self.app.store.subscription(ctx.target(), source_id).await
        else {
            return self.plain(ctx.message, true, "订阅标签设置失败!").await;
        };
        subscription.tag.clear();
        subscription.tag.push('#');
        for (i, tag) in args.take(3).enumerate() {
            if i > 0 {
                subscription.tag.push_str(" #");
            }
            subscription.tag.push_str(tag);
        }
        let text = if self
            .app
            .store
            .save_subscription(&subscription)
            .await
            .is_ok()
        {
            "订阅标签设置成功!"
        } else {
            "订阅标签设置失败!"
        };
        self.plain(ctx.message, true, text).await
    }

    async fn set_interval(&self, ctx: &CommandContext<'_>) -> Result<()> {
        let arguments = ctx.arguments();
        let mut args = arguments.trim().split(' ');
        let interval = args.next().and_then(|s| s.parse::<i64>().ok());
        if args.clone().next().is_none() {
            return self.plain(ctx.message, true, "/setinterval [interval] [sourceID] 设置订阅刷新频率（可设置多个sub id，以空格分割）").await;
        }
        let Some(interval) = interval.filter(|interval| *interval > 0) else {
            return self.plain(ctx.message, true, "请输入正确的抓取频率").await;
        };
        for id in args {
            let source_id = id.parse().unwrap_or(0);
            let Ok(Some(mut subscription)) =
                self.app.store.subscription(ctx.target(), source_id).await
            else {
                return self.plain(ctx.message, true, "抓取频率设置失败!").await;
            };
            subscription.interval = interval;
            if self
                .app
                .store
                .save_subscription(&subscription)
                .await
                .is_err()
            {
                return self.plain(ctx.message, true, "抓取频率设置失败!").await;
            }
        }
        self.plain(ctx.message, true, "抓取频率设置成功!").await
    }

    async fn set_active(&self, ctx: &CommandContext<'_>, active: bool) -> Result<()> {
        let Ok(subscriptions) = self.app.store.subscriptions(ctx.target()).await else {
            return self.plain(ctx.message, true, "系统错误").await;
        };
        let error_count = if active { 0 } else { self.paused_count() };
        for (source, _) in subscriptions {
            if self
                .app
                .store
                .set_error_count(source.id, error_count)
                .await
                .is_err()
            {
                let error = if active {
                    "激活失败"
                } else {
                    "暂停失败"
                };
                return self.plain(ctx.message, true, error).await;
            }
        }
        let status = if active {
            "订阅已全部开启"
        } else {
            "订阅已全部暂停"
        };
        let view = if let Some(chat) = &ctx.mentioned {
            View::markdown(format!(
                "频道 [{}](https://t.me/{}) {status}",
                chat.title().unwrap_or_default(),
                chat.username().unwrap_or_default()
            ))
        } else {
            View::markdown(status)
        };
        self.send(ctx.message, active, view).await
    }

    fn paused_count(&self) -> i64 {
        self.app.config.error_threshold.saturating_add(1)
    }
}

fn parse_command<'a>(text: &'a str, bot_username: &str) -> Option<(&'a str, &'a str)> {
    let text = text.strip_prefix('/')?;
    let (name, payload) = text
        .split_once(|c: char| c.is_ascii_whitespace())
        .unwrap_or((text, ""));
    let name = if let Some((name, username)) = name.split_once('@') {
        if username.is_empty() || !username.eq_ignore_ascii_case(bot_username) {
            return None;
        }
        name
    } else {
        name
    };
    COMMANDS
        .iter()
        .any(|(command, _)| *command == name)
        .then_some((name, payload.lines().next().unwrap_or("")))
}

fn entity_value<'a>(
    text: Option<&'a str>,
    entities: Option<&[MessageEntity]>,
    kind: MessageEntityKind,
) -> Option<&'a str> {
    let text = text?;
    entities?
        .iter()
        .filter(|entity| entity.kind == kind && entity.length > 0)
        .find_map(|entity| utf16_slice(text, entity.offset, entity.length))
}

fn utf16_slice(text: &str, offset: usize, length: usize) -> Option<&str> {
    let end = offset.checked_add(length)?;
    let mut units = 0;
    let mut start = None;
    for (index, c) in text
        .char_indices()
        .chain(std::iter::once((text.len(), '\0')))
    {
        if units == offset {
            start = Some(index);
        }
        if units == end {
            return Some(&text[start?..index]);
        }
        if units > end {
            break;
        }
        units += c.len_utf16();
    }
    None
}

fn mention(message: &Message) -> Option<&str> {
    entity_value(
        message.text(),
        message.entities(),
        MessageEntityKind::Mention,
    )
    .or_else(|| {
        entity_value(
            message.caption(),
            message.caption_entities(),
            MessageEntityKind::Mention,
        )
    })
}

fn find_url<'a>(ctx: &'a CommandContext<'_>) -> Option<&'a str> {
    entity_value(
        ctx.message.text(),
        ctx.message.entities(),
        MessageEntityKind::Url,
    )
    .or_else(|| {
        let mut args = ctx.payload.split_ascii_whitespace();
        let mut url = args.next()?;
        if Some(url) == ctx.mention_name {
            url = args.next()?;
        }
        (url.starts_with("http://") || url.starts_with("https://")).then_some(url)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_address_only_this_bot() {
        assert_eq!(
            parse_command("/sub@FlowerssBot https://example.com/", "flowerssbot"),
            Some(("sub", "https://example.com/"))
        );
        assert_eq!(parse_command("/ping@OtherBot", "flowerssbot"), None);
        assert_eq!(parse_command("/check", "flowerssbot"), None);
        assert_eq!(parse_command("/start", "flowerssbot"), Some(("start", "")));
    }

    #[test]
    fn slices_telegram_entities_by_utf16_offsets() {
        let text = "来🦀 @channel";
        assert_eq!(utf16_slice(text, 4, 8), Some("@channel"));
        assert_eq!(utf16_slice(text, 2, 1), None);
        assert_eq!(utf16_slice(text, 1, 1), None);
        assert_eq!(utf16_slice(text, 4, 9), None);
    }
}

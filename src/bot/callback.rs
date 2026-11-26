use anyhow::Result;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use prost::Message as _;
use teloxide_core::{
    payloads::AnswerCallbackQuerySetters,
    requests::{HasPayload, Requester},
    types::{CallbackQuery, CallbackQueryId, ChatId, InlineKeyboardButton, UserId},
};

use crate::store::{Source, Subscription};

use super::{Bot, CommandContext, View};

const CALLBACK_PREFIX: char = '\x0c';
const CALLBACK_SEPARATOR: char = '|';

#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    ConfirmUnsubAll,
    CancelUnsubAll,
    Unsubscribe,
    Settings,
    Notification,
    Tag,
    Update,
}

impl Action {
    const fn wire(self) -> &'static str {
        match self {
            Self::ConfirmUnsubAll => "unsub_all_confirm_btn",
            Self::CancelUnsubAll => "unsub_all_cancel_btn",
            Self::Unsubscribe => "unsub_feed_item_btn",
            Self::Settings => "set_feed_item_btn",
            Self::Notification => "set_toggle_notice_btn",
            Self::Tag => "set_set_sub_tag_btn",
            Self::Update => "set_toggle_update_btn",
        }
    }

    fn parse(data: &str) -> Option<(Self, &str)> {
        let data = data.strip_prefix(CALLBACK_PREFIX)?;
        let (name, payload) = data.split_once(CALLBACK_SEPARATOR).unwrap_or((data, ""));
        [
            Self::ConfirmUnsubAll,
            Self::CancelUnsubAll,
            Self::Unsubscribe,
            Self::Settings,
            Self::Notification,
            Self::Tag,
            Self::Update,
        ]
        .into_iter()
        .find(|action| action.wire() == name)
        .map(|action| (action, payload))
    }

    fn button(self, text: impl Into<String>, owner: i64, source: i64) -> InlineKeyboardButton {
        if owner == 0 {
            return InlineKeyboardButton::callback(
                text,
                format!("{CALLBACK_PREFIX}{}", self.wire()),
            );
        }
        let attachment = Attachment { owner, source };
        let data = attachment.encode_to_vec();
        let payload = if u32::try_from(source).is_ok() {
            // Same hexadecimal proto3 varints as the legacy Go Attachment.
            hex::encode(data)
        } else {
            // Keep 64-bit source IDs within Telegram's 64-byte callback limit.
            format!("~{}", URL_SAFE_NO_PAD.encode(data))
        };
        InlineKeyboardButton::callback(
            text,
            format!(
                "{CALLBACK_PREFIX}{}{CALLBACK_SEPARATOR}{payload}",
                self.wire()
            ),
        )
    }
}

#[derive(Clone, Copy, PartialEq, Eq, prost::Message)]
struct Attachment {
    #[prost(int64, tag = "1")]
    owner: i64,
    #[prost(int64, tag = "2")]
    source: i64,
}

impl Attachment {
    fn parse(payload: &str) -> Option<Self> {
        let bytes = if let Some(payload) = payload.strip_prefix('~') {
            URL_SAFE_NO_PAD.decode(payload).ok()?
        } else {
            hex::decode(payload).ok()?
        };
        let attachment = Self::decode(bytes.as_slice()).ok()?;
        (attachment.owner != 0 && attachment.source >= 0).then_some(attachment)
    }
}

struct Response {
    answer: Option<&'static str>,
    view: Option<View>,
    edit: bool,
}

impl Response {
    fn answer(answer: Option<&'static str>) -> Self {
        Self {
            answer,
            view: None,
            edit: false,
        }
    }

    fn edit(view: View) -> Self {
        Self {
            answer: None,
            view: Some(view),
            edit: true,
        }
    }

    fn send(text: &str) -> Self {
        Self {
            answer: None,
            view: Some(View::plain(text)),
            edit: false,
        }
    }
}

impl Bot<'_> {
    pub(super) async fn pick_setting(&self, ctx: &CommandContext<'_>) -> Result<()> {
        self.pick_source(
            ctx,
            Action::Settings,
            "请选择你要设置的源",
            "获取订阅失败",
            "当前没有订阅",
        )
        .await
    }

    pub(super) async fn pick_unsubscribe(&self, ctx: &CommandContext<'_>) -> Result<()> {
        self.pick_source(
            ctx,
            Action::Unsubscribe,
            "请选择你要退订的源",
            "获取订阅列表失败",
            "没有订阅",
        )
        .await
    }

    async fn pick_source(
        &self,
        ctx: &CommandContext<'_>,
        action: Action,
        prompt: &str,
        error: &str,
        empty: &str,
    ) -> Result<()> {
        let target = ctx.target();
        let Ok(subscriptions) = self.app.store.subscriptions(target).await else {
            return self.plain(ctx.message, true, error).await;
        };
        if subscriptions.is_empty() {
            return self.plain(ctx.message, true, empty).await;
        }
        let keys = subscriptions
            .into_iter()
            .map(|(source, _)| {
                vec![action.button(
                    format!("[{}] {}", source.id, source.title),
                    target,
                    source.id,
                )]
            })
            .collect();
        self.send(ctx.message, true, View::plain(prompt).keyboard(keys))
            .await
    }

    pub(super) async fn confirm_unsubscribe_all(&self, ctx: &CommandContext<'_>) -> Result<()> {
        let owner = if ctx.message.chat.is_private() && ctx.mentioned.is_none() {
            0 // The original private confirmation carries no attachment.
        } else {
            ctx.target()
        };
        let keys = vec![vec![
            Action::ConfirmUnsubAll.button("确认", owner, 0),
            Action::CancelUnsubAll.button("取消", owner, 0),
        ]];
        self.send(
            ctx.message,
            true,
            View::plain("是否退订当前用户的所有订阅？").keyboard(keys),
        )
        .await
    }

    pub(super) async fn callback(&self, callback: CallbackQuery) -> Result<()> {
        let Some((action, payload)) = callback.data.as_deref().and_then(Action::parse) else {
            return Ok(());
        };
        let Some(message) = &callback.message else {
            return self.answer_callback(callback.id, None).await;
        };
        let sender = callback.from.id;
        let chat = message.chat();
        let message_id = message.id();
        if !self.allowed(sender) {
            return self.answer_callback(callback.id, None).await;
        }
        if !self.is_admin(chat.id, chat.is_private(), sender).await {
            let answer = self.answer_callback(callback.id, None).await;
            return self
                .send_view(
                    chat.id,
                    (message_id.0 != 0).then_some(message_id),
                    View::plain("您不是当前会话的管理员"),
                )
                .await
                .and(answer);
        }

        let response = self
            .callback_response(action, payload, sender, chat.id)
            .await;
        let answer = self.answer_callback(callback.id, response.answer).await;
        let changed = match response.view {
            Some(view) if response.edit => {
                let mut request = self
                    .telegram
                    .edit_message_text(chat.id, message_id, view.text);
                let payload = request.payload_mut();
                payload.parse_mode = view.mode;
                payload.reply_markup = view.keyboard;
                request.await.map(|_| ()).map_err(Into::into)
            }
            Some(view) => self.send_view(chat.id, None, view).await,
            None => Ok(()),
        };
        changed.and(answer)
    }

    async fn answer_callback(&self, id: CallbackQueryId, text: Option<&str>) -> Result<()> {
        let mut request = self.telegram.answer_callback_query(id);
        if let Some(text) = text {
            request = request.text(text);
        }
        request.await?;
        Ok(())
    }

    async fn callback_response(
        &self,
        action: Action,
        payload: &str,
        sender: UserId,
        current_chat: ChatId,
    ) -> Response {
        let bad_payload = match action {
            Action::ConfirmUnsubAll => Response::edit(View::plain("退订失败")),
            Action::CancelUnsubAll => Response::edit(View::plain("操作取消")),
            Action::Unsubscribe | Action::Settings => Response::edit(View::plain("退订错误！")),
            Action::Notification | Action::Tag => Response::edit(View::plain("系统错误！")),
            Action::Update => Response::answer(Some("error")),
        };
        let attachment = if payload.is_empty()
            && matches!(action, Action::ConfirmUnsubAll | Action::CancelUnsubAll)
        {
            // Existing confirmation messages carry no attachment; Go acted on the sender.
            Attachment {
                owner: ChatId::from(sender).0,
                source: 0,
            }
        } else if let Some(attachment) = Attachment::parse(payload) {
            attachment
        } else {
            return bad_payload;
        };
        let Attachment { owner, source } = attachment;
        if !self.can_manage_target(owner, sender, current_chat).await {
            return match action {
                Action::Settings => Response::edit(View::plain("获取订阅信息失败")),
                Action::Tag => Response::send("无权限"),
                _ => Response::answer(Some("error")),
            };
        }
        match action {
            Action::ConfirmUnsubAll => {
                let text = if self.unsubscribe_all(owner).await.is_ok() {
                    "退订成功"
                } else {
                    "退订失败"
                };
                Response::edit(View::plain(text))
            }
            Action::CancelUnsubAll => Response::edit(View::plain("操作取消")),
            Action::Unsubscribe => {
                let Ok(Some(feed)) = self.app.store.source(source).await else {
                    return bad_payload;
                };
                if !matches!(
                    self.app.store.remove_subscription(owner, source).await,
                    Ok(true)
                ) {
                    return bad_payload;
                }
                Response::edit(View::html(format!(
                    "[{source}] <a href=\"{}\">{}</a> 退订成功",
                    html_escape::encode_double_quoted_attribute(&feed.link),
                    html_escape::encode_text(&feed.title),
                )))
            }
            Action::Tag => Response::edit(View::markdown(format!(
                "请使用`/setfeedtag {source} tags`命令为该订阅设置标签，tags为需要设置的标签，以空格分隔。（最多设置三个标签） \n例如：`/setfeedtag {source} 科技 苹果`"
            ))),
            Action::Settings | Action::Notification | Action::Update => {
                self.change_setting(action, owner, source).await
            }
        }
    }

    async fn can_manage_target(&self, owner: i64, sender: UserId, current_chat: ChatId) -> bool {
        if owner == ChatId::from(sender).0 || owner == current_chat.0 {
            // Middleware has already confirmed administrator rights in the current chat.
            return true;
        }
        if owner > 0 {
            return false;
        }
        match self.telegram.get_chat(ChatId(owner)).await {
            Ok(chat) if chat.id.0 == owner && !chat.is_private() => {
                self.is_admin(chat.id, false, sender).await
            }
            _ => false,
        }
    }

    async fn unsubscribe_all(&self, owner: i64) -> Result<()> {
        for (source, _) in self.app.store.subscriptions(owner).await? {
            self.app.store.remove_subscription(owner, source.id).await?;
        }
        Ok(())
    }

    async fn change_setting(&self, action: Action, owner: i64, source: i64) -> Response {
        let Ok(Some(mut feed)) = self.app.store.source(source).await else {
            return if action == Action::Settings {
                Response::edit(View::plain("找不到该订阅源"))
            } else {
                Response::answer(Some("error"))
            };
        };
        let Ok(Some(mut sub)) = self.app.store.subscription(owner, source).await else {
            return if action == Action::Settings {
                Response::edit(View::plain("用户未订阅该rss"))
            } else {
                Response::answer(Some("error"))
            };
        };
        let threshold = self.app.config.error_threshold;
        let changed = match action {
            Action::Settings => true,
            Action::Notification => {
                sub.enable_notification = !sub.enable_notification;
                self.app.store.save_subscription(&sub).await.is_ok()
            }
            Action::Update => {
                feed.error_count = if feed.error_count < threshold {
                    self.paused_count()
                } else {
                    0
                };
                self.app
                    .store
                    .set_error_count(source, feed.error_count)
                    .await
                    .is_ok()
            }
            _ => unreachable!("action is not a feed setting"),
        };
        if !changed {
            return Response::answer(Some("error"));
        }
        Response {
            answer: (action != Action::Settings).then_some("修改成功"),
            ..Response::edit(setting_view(&feed, &sub, threshold))
        }
    }
}

fn setting_view(feed: &Source, sub: &Subscription, threshold: i64) -> View {
    let paused = feed.error_count >= threshold;
    let status = if paused { "暂停" } else { "抓取中" };
    let notification = if sub.enable_notification {
        "开启"
    } else {
        "关闭"
    };
    let tag = if sub.tag.is_empty() { "无" } else { &sub.tag };
    let text = format!(
        "\n订阅<b>设置</b>\n[id] {}\n[标题] {}\n[Link] {}\n[抓取更新] {}\n[抓取频率] {}分钟\n[通知] {}\n[Tag] {}\n",
        feed.id,
        html_escape::encode_text(&feed.title),
        html_escape::encode_text(&feed.link),
        status,
        sub.interval,
        notification,
        html_escape::encode_text(tag),
    );
    let (owner, source) = (sub.user_id, feed.id);
    let keys = vec![
        vec![
            Action::Update.button(
                if paused {
                    "重启更新"
                } else {
                    "暂停更新"
                },
                owner,
                source,
            ),
            Action::Notification.button(
                if sub.enable_notification {
                    "关闭通知"
                } else {
                    "开启通知"
                },
                owner,
                source,
            ),
        ],
        vec![Action::Tag.button("标签设置", owner, source)],
    ];
    View::html(text).keyboard(keys)
}

#[cfg(test)]
mod tests {
    use super::*;
    use teloxide_core::types::InlineKeyboardButtonKind;

    fn callback_data(button: &InlineKeyboardButton) -> &str {
        let InlineKeyboardButtonKind::CallbackData(data) = &button.kind else {
            panic!("expected callback button");
        };
        data
    }

    #[test]
    fn retains_legacy_wire_format() {
        let data = "\x0cset_feed_item_btn|087b10c102";
        let (action, payload) = Action::parse(data).unwrap();
        let attachment = Attachment::parse(payload).unwrap();
        assert_eq!((attachment.owner, attachment.source), (123, 321));
        assert_eq!(
            callback_data(&action.button("feed", attachment.owner, attachment.source)),
            data
        );
        let (action, payload) = Action::parse("\x0cunsub_all_confirm_btn").unwrap();
        assert!(action == Action::ConfirmUnsubAll && payload.is_empty());
    }

    #[test]
    fn large_callback_ids_remain_usable() {
        for source in [i64::from(u32::MAX), i64::MAX] {
            let button = Action::Update.button("button", -1001234567890, source);
            assert!(callback_data(&button).len() <= 64);
            let (_, payload) = Action::parse(callback_data(&button)).unwrap();
            let attachment = Attachment::parse(payload).unwrap();
            assert_eq!(
                (attachment.owner, attachment.source),
                (-1001234567890, source)
            );
        }
    }
}

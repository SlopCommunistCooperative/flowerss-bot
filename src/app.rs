use std::time::Duration;

use anyhow::Result;
use teloxide_core::{
    Bot as Telegram,
    payloads::SendMessageSetters,
    requests::{HasPayload, Requester},
    types::{ChatId, ParseMode},
};

use crate::{
    config::{Config, MessageMode},
    feed::{self, Item},
    message::{self, Message, MessageTemplate},
    store::{Content, Source, Store},
    telegram::{MARKDOWN, disable_link_preview, is_forbidden},
    telegraph::Telegraph,
};

#[derive(Debug, thiserror::Error)]
#[error("subscription already exists")]
pub struct AlreadySubscribed;

#[derive(Debug, thiserror::Error)]
#[error("add subscription failed: {0}")]
pub struct AddSubscriptionError(#[source] anyhow::Error);

pub struct App {
    pub config: Config,
    pub store: Store,
    pub http: reqwest::Client,
    pub telegram_poll_http: reqwest::Client,
    template: MessageTemplate,
    telegraph: Telegraph,
}

impl App {
    pub async fn open(mut config: Config, template: MessageTemplate) -> Result<Self> {
        let proxy = if config.socks5.is_empty() {
            None
        } else {
            Some(reqwest::Proxy::all(format!("socks5h://{}", config.socks5))?)
        };
        let client = |timeout| {
            let builder = reqwest::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(timeout));
            match &proxy {
                Some(proxy) => builder.proxy(proxy.clone()),
                None => builder,
            }
            .build()
        };
        let http = client(10)?;
        // teloxide-core 0.13 does not apply its getUpdates timeout hint to reqwest.
        let telegram_poll_http = client(25)?;
        let store = Store::open(&config).await?;
        let telegraph = Telegraph::init(&http, &mut config).await;
        Ok(Self {
            config,
            store,
            http,
            telegram_poll_http,
            template,
            telegraph,
        })
    }

    pub async fn subscribe(&self, chat: i64, url: &str) -> Result<Source> {
        let source = match self.store.find_source(url).await? {
            Some(source) => source,
            None => {
                let feed = feed::fetch(&self.http, url, &self.config.user_agent).await?;
                // A source under construction must not be polled until its existing entries are recorded.
                let mut source = self
                    .store
                    .add_source(&feed.title, url, self.paused_count())
                    .await?;
                let seed = self.record_items(&source, feed.items, false).await;
                self.store.set_error_count(source.id, 0).await?;
                seed?;
                source.error_count = 0;
                source
            }
        };
        if !self
            .store
            .add_subscription(chat, source.id, self.config.update_interval)
            .await
            .map_err(AddSubscriptionError)?
        {
            return Err(AlreadySubscribed.into());
        }
        Ok(source)
    }

    fn paused_count(&self) -> i64 {
        self.config.error_threshold.saturating_add(1)
    }

    async fn record_items(
        &self,
        source: &Source,
        items: Vec<Item>,
        collect: bool,
    ) -> Result<Vec<Content>> {
        let mut inserted = Vec::new();
        for item in items {
            let hash_id = hash(&source.link, &item.id);
            if self.store.contains_content(&hash_id).await? {
                continue;
            }
            let mut title = item.title;
            title.truncate(title.trim_end_matches(' ').len());
            let start = title.len() - title.trim_start_matches(' ').len();
            title.drain(..start);
            let telegraph_url = if self.config.telegraph_enabled()
                && item.content.chars().nth(self.config.preview_text).is_some()
            {
                self.telegraph
                    .publish(
                        &self.http,
                        &self.config,
                        &source.title,
                        &title,
                        &item.link,
                        &item.content,
                    )
                    .await
            } else {
                String::new()
            };
            let content = Content {
                source_id: source.id,
                title,
                description: item.content,
                raw_id: item.id,
                hash_id,
                raw_link: item.link,
                telegraph_url,
            };
            self.store.add_content(&content).await?;
            if collect {
                inserted.push(content);
            }
        }
        Ok(inserted)
    }

    pub async fn schedule(&self, telegram: &Telegram) {
        let interval =
            Duration::from_secs((self.config.update_interval.max(0) as u64).saturating_mul(60));
        loop {
            if let Err(err) = self.scan(telegram).await {
                tracing::error!(%err, "get sources failed");
            }
            tokio::time::sleep(interval).await;
        }
    }

    async fn scan(&self, telegram: &Telegram) -> Result<()> {
        for source in self.store.sources().await? {
            if source.error_count < self.config.error_threshold
                && let Err(err) = self.update_source(telegram, &source).await
            {
                tracing::error!(%err, source_id = source.id, "update source failed");
            }
        }
        Ok(())
    }

    async fn update_source(&self, telegram: &Telegram, source: &Source) -> Result<()> {
        tracing::debug!(source_id = source.id, link = %source.link, "fetch source update");
        let feed = match feed::fetch(&self.http, &source.link, &self.config.user_agent).await {
            Ok(feed) => feed,
            Err(err) => {
                let errors = source.error_count + 1;
                self.store.set_error_count(source.id, errors).await?;
                if errors == self.config.error_threshold {
                    let text = format!(
                        "[{}]({}) 已经累计连续{errors}次更新失败，暂停更新",
                        source.title, source.link
                    );
                    for sub in self.store.subscribers(source.id).await? {
                        let result = telegram
                            .send_message(ChatId(sub.user_id), text.as_str())
                            .parse_mode(MARKDOWN)
                            .await;
                        if let Err(err) = result {
                            tracing::warn!(%err, chat = sub.user_id, "send source failure notice failed");
                        }
                    }
                }
                return Err(err);
            }
        };
        if source.error_count != 0 {
            self.store.set_error_count(source.id, 0).await?;
        }
        let contents = self.record_items(source, feed.items, true).await?;
        if contents.is_empty() {
            return Ok(());
        }
        let subscribers = self.store.subscribers(source.id).await?;
        let parse_mode = match self.config.message_mode {
            MessageMode::Html => Some(ParseMode::Html),
            MessageMode::Markdown => Some(MARKDOWN),
            MessageMode::Plain => None,
        };
        tracing::info!(
            source_id = source.id,
            subscribers = subscribers.len(),
            entries = contents.len(),
            "broadcast news"
        );
        for content in &contents {
            let preview = message::preview(&content.description, self.config.preview_text);
            for sub in &subscribers {
                let text = self.template.render(&Message {
                    source_title: &source.title,
                    content_title: &content.title,
                    raw_link: &content.raw_link,
                    preview: &preview,
                    telegraph_url: &content.telegraph_url,
                    tags: &sub.tag,
                    telegraph: sub.enable_telegraph && !content.telegraph_url.is_empty(),
                })?;
                let mut request = telegram.send_message(ChatId(sub.user_id), text);
                let payload = request.payload_mut();
                payload.parse_mode = parse_mode;
                payload.link_preview_options = self
                    .config
                    .disable_web_page_preview
                    .then(disable_link_preview);
                payload.disable_notification = Some(!sub.enable_notification);
                let result = request.await;
                if let Err(err) = result {
                    tracing::warn!(%err, chat = sub.user_id, source_id = source.id, "broadcast failed");
                    if is_forbidden(&err)
                        && let Err(err) =
                            self.store.remove_subscription(sub.user_id, source.id).await
                    {
                        tracing::error!(%err, chat = sub.user_id, source_id = source.id, "unsubscribe failed");
                    }
                }
            }
        }
        Ok(())
    }
}

fn hash(url: &str, id: &str) -> String {
    // Existing contents.hash_id values are 32-bit FNV-1, not FNV-1a.
    let sum = url
        .bytes()
        .chain(*b"||")
        .chain(id.bytes())
        .fold(0x811c9dc5_u32, |hash, byte| {
            hash.wrapping_mul(0x01000193) ^ u32::from(byte)
        });
    format!("{sum:08x}")
}

#[cfg(test)]
mod tests {
    use super::hash;

    #[test]
    fn matches_hashes_in_legacy_databases() {
        assert_eq!(
            hash(
                "http://www.ruanyifeng.com/blog/atom.xml",
                "tag:www.ruanyifeng.com,2019:/blog//1.2054"
            ),
            "96b2e254"
        );
        assert_eq!(
            hash(
                "https://rsshub.app/guokr/scientific",
                "https://www.guokr.com/article/445877/"
            ),
            "770fff44"
        );
    }
}

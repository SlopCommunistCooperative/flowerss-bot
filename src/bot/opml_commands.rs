use std::fmt::Write;

use anyhow::Result;
use chrono::Utc;

use teloxide_core::{
    requests::Requester,
    types::{Document, InputFile},
};

use crate::{app::AlreadySubscribed, opml, telegram};

use super::{Bot, CommandContext, View};

impl Bot<'_> {
    pub(super) async fn export_opml(&self, ctx: &CommandContext<'_>) -> Result<()> {
        let message = ctx.message;
        if ctx.mention_name.is_some() && ctx.mentioned.is_none() {
            return self.plain(message, false, "导出失败").await;
        }
        let target = ctx.target();
        let subscriptions = match self.app.store.subscriptions(target).await {
            Ok(subscriptions) => subscriptions,
            Err(error) => {
                tracing::warn!(%error, target, "loading subscriptions for OPML export failed");
                return self.plain(message, false, "导出失败").await;
            }
        };
        if subscriptions.is_empty() {
            return self.plain(message, false, "订阅列表为空").await;
        }

        let now = Utc::now();
        let file = opml::write(subscriptions.iter().map(|(source, _)| source), now);
        let result = match file {
            Ok(file) => {
                let name = format!("subscriptions_{}.opml", now.timestamp());
                let document = InputFile::memory(file.into_bytes()).file_name(name);
                self.telegram
                    .send_document(message.chat.id, document)
                    .await
                    .map(|_| ())
                    .map_err(Into::into)
            }
            Err(error) => Err(error),
        };
        if let Err(error) = result {
            tracing::warn!(%error, target, "sending OPML export failed");
            return self.plain(message, false, "导出失败").await;
        }
        Ok(())
    }

    pub(super) async fn import_opml(&self, ctx: &CommandContext<'_>) -> Result<()> {
        let message = ctx.message;
        let Some(document) = message.document() else {
            return Ok(());
        };
        if !document
            .file_name
            .as_deref()
            .is_some_and(|name| name.ends_with(".opml"))
        {
            return self.plain(message, true, "请发送正确的 OPML 文件").await;
        }
        let outlines = match self.read_opml(document).await {
            Ok(outlines) => outlines,
            Err(error) => {
                tracing::warn!(%error, "receiving OPML document failed");
                return self.plain(message, true, "获取文件失败").await;
            }
        };
        let target = ctx.target();
        let mut imported = ImportResult::default();
        let mut failed = ImportResult::default();
        for outline in outlines {
            let list = match self.app.subscribe(target, &outline.url).await {
                Ok(_) => &mut imported,
                Err(error) if error.is::<AlreadySubscribed>() => &mut imported,
                Err(error) => {
                    tracing::warn!(%error, target, url = %outline.url, "OPML subscription failed");
                    &mut failed
                }
            };
            list.push(&outline);
        }
        let mut summary = format!(
            "<b>导入成功：{}，导入失败：{}</b>\n",
            imported.count, failed.count
        );
        if imported.count > 0 {
            summary.push_str("<b>以下订阅源导入成功:</b>\n");
            summary.push_str(&imported.text);
            summary.push('\n');
        }
        if failed.count > 0 {
            summary.push_str("<b>以下订阅源导入失败:</b>\n");
            summary.push_str(&failed.text);
        }
        self.send(message, true, View::html(summary)).await
    }

    async fn read_opml(&self, document: &Document) -> Result<Vec<opml::Outline>> {
        let data = telegram::download(self.telegram, document.file.id.clone()).await?;
        opml::read(data.as_ref())
    }
}

#[derive(Default)]
struct ImportResult {
    count: usize,
    text: String,
}

impl ImportResult {
    fn push(&mut self, outline: &opml::Outline) {
        self.count += 1;
        if outline.text.is_empty() {
            let _ = writeln!(
                self.text,
                "[{}] {}",
                self.count,
                html_escape::encode_text(&outline.url)
            );
        } else {
            let _ = writeln!(
                self.text,
                "[{}] <a href=\"{}\">{}</a>",
                self.count,
                html_escape::encode_double_quoted_attribute(&outline.url),
                html_escape::encode_text(&outline.text)
            );
        }
    }
}

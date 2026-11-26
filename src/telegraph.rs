use std::{
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

use anyhow::{Result, bail};
use scraper::{ElementRef, Html, node::Node};
use serde::{Deserialize, Serialize};

use crate::config::Config;

const ENDPOINT: &str = "https://api.telegra.ph";

pub struct Telegraph {
    next: AtomicUsize,
}

impl Telegraph {
    pub async fn init(http: &reqwest::Client, config: &mut Config) -> Self {
        let configured = std::mem::take(&mut config.telegraph_token);
        if configured.is_empty() && !config.telegraph_account.is_empty() {
            let result = call::<Account>(
                http,
                "createAccount",
                &[
                    ("short_name", &config.telegraph_account),
                    ("author_name", &config.telegraph_author_name),
                    ("author_url", &config.telegraph_author_url),
                ],
            )
            .await;
            match result {
                Ok(account) => config.telegraph_token.push(account.access_token),
                Err(err) => tracing::warn!(%err, "create Telegraph account failed"),
            }
        } else {
            for token in configured {
                match call::<serde_json::Value>(
                    http,
                    "getAccountInfo",
                    &[("access_token", token.as_str())],
                )
                .await
                {
                    Ok(_) => config.telegraph_token.push(token),
                    Err(err) => tracing::warn!(%err, "load Telegraph token failed"),
                }
            }
        }
        Self {
            next: AtomicUsize::new(0),
        }
    }

    pub async fn publish(
        &self,
        http: &reqwest::Client,
        config: &Config,
        source_title: &str,
        title: &str,
        link: &str,
        content: &str,
    ) -> String {
        let index = self.next.fetch_add(1, Ordering::Relaxed) % config.telegraph_token.len();
        let result = async {
            let decoded = html_escape::decode_html_entities(content);
            let content = content_json(&decoded)?;
            let page = call::<Page>(
                http,
                "createPage",
                &[
                    ("access_token", &config.telegraph_token[index]),
                    ("title", &format!("{title} - {source_title}")),
                    ("author_name", source_title),
                    ("author_url", link),
                    ("content", &content),
                    ("return_content", "true"),
                ],
            )
            .await?;
            Ok::<_, anyhow::Error>(page.url)
        }
        .await;
        match result {
            Ok(url) => url,
            Err(err) => {
                tracing::warn!(%err, "create Telegraph page failed");
                String::new()
            }
        }
    }
}

#[derive(Deserialize)]
struct Account {
    access_token: String,
}

#[derive(Deserialize)]
struct Page {
    url: String,
}

async fn call<T: serde::de::DeserializeOwned>(
    http: &reqwest::Client,
    method: &str,
    form: &[(&str, &str)],
) -> Result<T> {
    #[derive(Deserialize)]
    struct Response<T> {
        ok: bool,
        result: Option<T>,
        error: Option<String>,
    }
    let response = http
        .post(format!("{ENDPOINT}/{method}"))
        .timeout(Duration::from_secs(10))
        .form(form)
        .send()
        .await?
        .error_for_status()?
        .json::<Response<T>>()
        .await?;
    if let Some(result) = response.result.filter(|_| response.ok) {
        return Ok(result);
    }
    bail!(
        "{}",
        response
            .error
            .as_deref()
            .unwrap_or("invalid Telegraph API response")
    );
}

#[derive(Serialize)]
#[serde(untagged)]
enum ContentNode<'a> {
    Text(&'a str),
    Element {
        tag: &'a str,
        #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
        attrs: std::collections::BTreeMap<&'a str, &'a str>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        children: Vec<ContentNode<'a>>,
    },
}

fn content_json(content: &str) -> Result<String> {
    fn nodes(root: ElementRef<'_>) -> Vec<ContentNode<'_>> {
        root.children()
            .filter_map(|child| match child.value() {
                Node::Text(text) => Some(ContentNode::Text(text)),
                Node::Element(el) => Some(ContentNode::Element {
                    tag: el.name(),
                    attrs: el.attrs().collect(),
                    children: nodes(ElementRef::wrap(child).expect("HTML element")),
                }),
                _ => None,
            })
            .collect()
    }
    let fragment = Html::parse_fragment(content);
    Ok(serde_json::to_string(&nodes(fragment.root_element()))?)
}

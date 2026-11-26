use std::fmt::Write as _;

use html_escape::{encode_double_quoted_attribute, encode_text};
use reqwest::Url;
use scraper::{ElementRef, Html, node::Node};

use crate::store::{Content, Source};

const DEFAULT_SUMMARY_CHARS: usize = 300;
// Leave room in Telegram's 32768-character limit for the heading, tags and summary.
const BODY_CHARS: usize = 30_000;
const METADATA_CHARS: usize = 500;

pub(crate) struct Article {
    heading: String,
    parts: Vec<String>,
}

impl Article {
    pub(crate) fn new(source: &Source, content: &Content, preview_text: usize) -> Self {
        let base = Url::parse(&source.link).ok();
        let original = web_url(base.as_ref(), &content.raw_link);
        let title = match content.title.trim() {
            "" => "RSS 更新",
            title => title,
        };
        let mut heading = format!(
            "<p><b>{}</b></p><h3>",
            encode_text(clip(&source.title, METADATA_CHARS))
        );
        write_link(&mut heading, original.as_ref(), clip(title, METADATA_CHARS));
        heading.push_str("</h3>");

        let document = Html::parse_fragment(&content.description);
        let mut body = Body::default();
        body.render(
            document.root_element(),
            original.as_ref().or(base.as_ref()),
            0,
            false,
        );
        let text = body.text.trim();
        let summary_chars = if preview_text == 0 {
            DEFAULT_SUMMARY_CHARS
        } else {
            preview_text.min(1000)
        };
        let summary: String = text
            .split_whitespace()
            .flat_map(|word| word.chars().chain([' ']))
            .take(summary_chars + 2)
            .collect();
        let summary = summary.trim_end();
        let excerpt = clip(summary, summary_chars);

        let mut parts = Vec::new();
        if body.elements <= 490 && clip(&body.text, BODY_CHARS).len() == body.text.len() {
            parts.push(body.html);
        } else {
            // Preserve all article text when it cannot fit as formatted HTML in one message.
            let mut rest = text;
            while !rest.is_empty() {
                let chunk = clip(rest, BODY_CHARS);
                parts.push(format!(
                    "<p>{}</p>",
                    encode_text(chunk).replace('\n', "<br>")
                ));
                rest = &rest[chunk.len()..];
            }
        }
        if parts.is_empty() {
            parts.push(String::new());
        }
        if parts.len() > 1 || excerpt.len() < summary.len() {
            let first = std::mem::take(&mut parts[0]);
            let label = if parts.len() == 1 {
                "展开全文"
            } else {
                "展开正文（第 1 部分）"
            };
            parts[0] = format!(
                "<p>{}…</p><details><summary>{label}</summary>{first}</details>",
                encode_text(excerpt.trim_end()),
            );
            for (index, part) in parts.iter_mut().enumerate().skip(1) {
                *part = format!(
                    "<details><summary>展开正文（第 {} 部分）</summary>{part}</details>",
                    index + 1
                );
            }
        }
        Self { heading, parts }
    }

    pub(crate) fn messages<'a>(&'a self, tags: &'a str) -> impl Iterator<Item = String> + 'a {
        self.parts.iter().enumerate().map(move |(index, part)| {
            let mut html = String::with_capacity(self.heading.len() + part.len() + tags.len());
            html.push_str(&self.heading);
            if index == 0 && !tags.is_empty() {
                write!(html, "<p>{}</p>", encode_text(clip(tags, METADATA_CHARS))).unwrap();
            }
            html.push_str(part);
            html
        })
    }
}

#[derive(Default)]
struct Body {
    html: String,
    text: String,
    elements: usize,
}

impl Body {
    fn render(&mut self, parent: ElementRef<'_>, base: Option<&Url>, depth: usize, in_link: bool) {
        for node in parent.children() {
            match node.value() {
                Node::Text(text) => {
                    let text: &str = text;
                    self.text.push_str(text);
                    html_escape::encode_text_to_string(text, &mut self.html);
                }
                Node::Element(el) => {
                    let name = el.name();
                    let element = ElementRef::wrap(node).expect("HTML element");
                    match name {
                        "script" | "style" | "template" | "noscript" => continue,
                        "br" | "hr" => {
                            self.text.push('\n');
                            self.elements += 1;
                            write!(self.html, "<{name}>").unwrap();
                            continue;
                        }
                        // Refer to external media without requiring Telegram to fetch it to deliver the article.
                        "img" | "audio" | "video" | "source" => {
                            let url = el.attr("src").and_then(|src| web_url(base, src));
                            let default_label = match name {
                                "img" => "查看图片",
                                "audio" => "收听音频",
                                "video" => "观看视频",
                                _ => "打开媒体",
                            };
                            let label = el.attr("alt").filter(|alt| !alt.is_empty());
                            if let Some(label) = label.or(url.as_ref().map(|_| default_label)) {
                                self.text.push(' ');
                                self.text.push_str(label);
                                self.text.push(' ');
                                self.elements += 1;
                                write_link(&mut self.html, url.as_ref().filter(|_| !in_link), label);
                            }
                            continue;
                        }
                        _ => {}
                    }
                    let tag = match name {
                        "p" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "ul" | "ol" | "li"
                        | "blockquote" | "pre" | "b" | "strong" | "i" | "em" | "u" | "s"
                        | "del" | "code" | "mark" | "sub" | "sup" => Some(name),
                        _ => None,
                    };
                    let url = if name == "a" && !in_link && depth < 12 {
                        el.attr("href").and_then(|href| web_url(base, href))
                    } else {
                        None
                    };
                    let tag = tag.or(url.as_ref().map(|_| "a")).filter(|_| depth < 12);
                    let boundary = matches!(
                        name,
                        "p" | "div" | "section" | "article" | "li" | "pre" | "blockquote"
                            | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "tr" | "td" | "th"
                    );
                    if boundary {
                        self.text.push('\n');
                        self.html.push('\n');
                    }
                    if let Some(tag) = tag {
                        self.elements += 1;
                        if let Some(url) = url {
                            write!(
                                self.html,
                                "<a href=\"{}\">",
                                encode_double_quoted_attribute(url.as_str())
                            )
                            .unwrap();
                        } else {
                            write!(self.html, "<{tag}>").unwrap();
                        }
                    }
                    self.render(
                        element,
                        base,
                        depth + usize::from(tag.is_some()),
                        in_link || tag == Some("a"),
                    );
                    if let Some(tag) = tag {
                        write!(self.html, "</{tag}>").unwrap();
                    }
                    if boundary {
                        self.text.push('\n');
                        self.html.push('\n');
                    }
                }
                _ => {}
            }
        }
    }
}

fn clip(text: &str, chars: usize) -> &str {
    let end = text
        .char_indices()
        .nth(chars)
        .map_or(text.len(), |(index, _)| index);
    &text[..end]
}

fn web_url(base: Option<&Url>, link: &str) -> Option<Url> {
    if link.trim().is_empty() {
        return None;
    }
    Url::options()
        .base_url(base)
        .parse(link)
        .ok()
        .filter(|url| matches!(url.scheme(), "http" | "https"))
}

fn write_link(html: &mut String, url: Option<&Url>, label: &str) {
    let label = encode_text(label);
    if let Some(url) = url {
        write!(
            html,
            "<a href=\"{}\">{label}</a>",
            encode_double_quoted_attribute(url.as_str())
        )
        .unwrap();
    } else {
        html.push_str(&label);
    }
}

use anyhow::Result;
use gtmpl_ng::{Context, Template};
use gtmpl_value_ng::Value;
use scraper::{Html, node::Node};

use crate::config::{Config, MessageMode};

const DEFAULT_TEMPLATE: &str = r#"<b>{{.SourceTitle}}</b>{{ if .PreviewText }}
---------- Preview ----------
{{.PreviewText}}
-----------------------------
{{- end}}{{if .EnableTelegraph}}
{{.ContentTitle}} <a href="{{.TelegraphURL}}">Telegraph</a> | <a href="{{.RawLink}}">原文</a>
{{- else }}
<a href="{{.RawLink}}">{{.ContentTitle}}</a>
{{- end }}
{{.Tags}}
"#;

pub struct MessageTemplate {
    template: Template,
    mode: MessageMode,
}

pub struct Message<'a> {
    pub source_title: &'a str,
    pub content_title: &'a str,
    pub raw_link: &'a str,
    pub preview: &'a str,
    pub telegraph_url: &'a str,
    pub tags: &'a str,
    pub telegraph: bool,
}

impl MessageTemplate {
    pub fn new(config: &Config) -> Result<Self> {
        let mut template = Template::with_name("message");
        template.parse(config.message_tpl.as_deref().unwrap_or(DEFAULT_TEMPLATE))?;
        Ok(Self {
            template,
            mode: config.message_mode,
        })
    }

    pub fn render(&self, message: &Message<'_>) -> Result<String> {
        let escape = |text: &str| match self.mode {
            MessageMode::Html => Value::from(html_escape::encode_double_quoted_attribute(text)),
            MessageMode::Plain => Value::from(text),
            MessageMode::Markdown => {
                let mut escaped = String::with_capacity(text.len());
                for ch in text.chars() {
                    if matches!(ch, '[' | '*' | '`' | '_') {
                        escaped.push('\\');
                    }
                    escaped.push(ch);
                }
                Value::from(escaped)
            }
        };
        let context = Context::from(Value::Object(
            [
                ("SourceTitle".into(), escape(message.source_title)),
                ("ContentTitle".into(), escape(message.content_title)),
                ("RawLink".into(), Value::from(message.raw_link)),
                ("PreviewText".into(), escape(message.preview)),
                ("TelegraphURL".into(), Value::from(message.telegraph_url)),
                ("Tags".into(), Value::from(message.tags)),
                ("EnableTelegraph".into(), Value::from(message.telegraph)),
            ]
            .into(),
        ));
        let mut rendered = self.template.render(&context)?;
        rendered.truncate(rendered.trim_end().len());
        let start = rendered.len() - rendered.trim_start().len();
        rendered.drain(..start);
        Ok(rendered)
    }
}

pub fn preview(content: &str, limit: usize) -> String {
    if limit == 0 || content.is_empty() {
        return String::new();
    }
    let decoded = html_escape::decode_html_entities(content);
    let mut result = String::new();
    let mut remaining = limit;
    let mut line_break = false;
    // Parse the fragment once. Text elements preserve line breaks; br produces another one.
    let html = Html::parse_fragment(&decoded);
    'preview: for node in html.tree.root().descendants() {
        let chars = match node.value() {
            Node::Text(text) => text.chars(),
            Node::Element(el) if el.name() == "br" => "\n".chars(),
            _ => continue,
        };
        for ch in chars {
            if ch == '\n' {
                line_break = !result.is_empty();
                continue;
            }
            if line_break {
                result.push('\n');
                remaining -= 1;
                if remaining == 0 {
                    break 'preview;
                }
                line_break = false;
            }
            result.push(ch);
            remaining -= 1;
            if remaining == 0 {
                break 'preview;
            }
        }
    }
    result
}

pub fn validate(template: &MessageTemplate) {
    for (source_title, preview, tags, telegraph_url) in [
        ("RSS 源标识 - 无预览无telegraph的消息", "", "", ""),
        (
            "RSS源标识 - 有预览无telegraph的消息",
            "这里是很长很长很长的消息预览字数补丁紫薯补丁紫薯补丁紫薯补丁紫薯补丁[1](123)",
            "#标签",
            "",
        ),
        (
            "RSS源标识 - 有预览有telegraph的消息",
            "这里是很长很长很长的消息预览字数补丁紫薯补丁紫薯补丁紫薯补丁紫薯补丁",
            "#标签1 #标签2",
            "https://telegra.ph/markdown-07-07",
        ),
    ] {
        println!("\n////////////////////////////////////////////");
        let msg = Message {
            source_title,
            content_title: "这是标题",
            raw_link: "https://www.github.com/",
            preview,
            telegraph_url,
            tags,
            telegraph: !telegraph_url.is_empty(),
        };
        match template.render(&msg) {
            Ok(rendered) => println!("{rendered} <nil>"),
            Err(err) => println!(" {err}"),
        }
    }
    println!("\n////////////////////////////////////////////");
}

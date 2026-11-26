use anyhow::{Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use encoding_rs::Encoding;
use feed_rs::model::FeedType;
use quick_xml::{Reader, events::Event};
use serde::Deserialize;

#[derive(Debug)]
pub struct Feed {
    pub title: String,
    pub items: Vec<Item>,
}

#[derive(Debug)]
pub struct Item {
    pub title: String,
    pub id: String,
    pub link: String,
    pub content: String,
}

pub async fn fetch(http: &reqwest::Client, url: &str, user_agent: &str) -> Result<Feed> {
    let mut request = http.get(url).timeout(std::time::Duration::from_secs(10));
    if !user_agent.is_empty() {
        request = request.header(reqwest::header::USER_AGENT, user_agent);
    }
    let response = request.send().await?;
    if !response.status().is_success() {
        bail!("{}", response.status());
    }
    parse(&response.bytes().await?)
}

fn parse(bytes: &[u8]) -> Result<Feed> {
    // Both parsers accept the original encoding from the XML declaration. Decode
    // UTF-16 first because their byte-oriented root detection cannot see its tags.
    let utf16;
    let bytes = if let Some((encoding, 2)) = Encoding::for_bom(bytes) {
        utf16 = encoding.decode(bytes).0.into_owned();
        // The bytes no longer match the declaration.
        utf16
            .strip_prefix("<?xml")
            .and_then(|_| utf16.split_once("?>").map(|(_, rest)| rest))
            .unwrap_or(&utf16)
            .as_bytes()
    } else {
        bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes)
    };
    let bytes = bytes.trim_ascii_start();
    if bytes.starts_with(b"{") {
        return parse_json(std::str::from_utf8(bytes)?);
    }

    // RSS needs a format-specific reader: the common feed-rs model drops RSS 1.0
    // entries without links and does not expose RSS 1.0 GUIDs.
    let mut xml = Reader::from_reader(bytes);
    loop {
        match xml.read_event()? {
            Event::Start(element) | Event::Empty(element) => {
                return match element.local_name().as_ref() {
                    "rss" | "RDF" => parse_rss(bytes),
                    "feed" => parse_atom(bytes),
                    _ => bail!("feed type not detected"),
                };
            }
            Event::Eof => bail!("feed type not detected"),
            _ => {}
        }
    }
}

fn parse_rss(bytes: &[u8]) -> Result<Feed> {
    let rss::Channel {
        mut title,
        items,
        dublin_core_ext,
        namespaces,
        ..
    } = rss::Channel::read_from(bytes)?;
    if title.is_empty() {
        title = dublin_core_ext
            .and_then(|dc| dc.titles.into_iter().next())
            .unwrap_or_default();
    }
    let content_prefixes = namespaces
        .iter()
        .filter(|(prefix, uri)| {
            *prefix != "content" && *uri == "http://purl.org/rss/1.0/modules/content/"
        })
        .map(|(prefix, _)| prefix);
    Ok(Feed {
        title,
        items: items
            .into_iter()
            .map(|mut item| {
                let content = item
                    .content
                    .or_else(|| {
                        content_prefixes.clone().find_map(|prefix| {
                            item.extensions
                                .remove(prefix)?
                                .remove("encoded")?
                                .into_iter()
                                .next()?
                                .value
                        })
                    })
                    .unwrap_or_default();
                Item {
                    title: item
                        .title
                        .or_else(|| item.dublin_core_ext?.titles.into_iter().next())
                        .unwrap_or_default(),
                    id: item.guid.map(|guid| guid.value).unwrap_or_default(),
                    link: item.link.unwrap_or_default(),
                    content,
                }
            })
            .collect(),
    })
}

fn parse_atom(bytes: &[u8]) -> Result<Feed> {
    let atom = feed_rs::parser::Builder::new()
        // Empty IDs are part of the existing database's deduplication contract.
        .id_generator(|_, _, _| String::new())
        .sanitize_content(false)
        .build()
        .parse(bytes)?;
    if atom.feed_type != FeedType::Atom {
        bail!("feed type not detected");
    }
    Ok(Feed {
        title: atom.title.map(|title| title.content).unwrap_or_default(),
        items: atom
            .entries
            .into_iter()
            .map(|mut entry| {
                entry.id.truncate(entry.id.trim_end().len());
                entry
                    .id
                    .drain(..entry.id.len() - entry.id.trim_start().len());
                let content = entry.content.and_then(|content| {
                    let body = content.body?;
                    let mime = content.content_type.as_str();
                    if mime.starts_with("text/") || mime == "application/xhtml+xml" {
                        Some(body)
                    } else {
                        Some(
                            STANDARD
                                .decode(body.trim())
                                .ok()
                                .and_then(|bytes| String::from_utf8(bytes).ok())
                                .unwrap_or(body),
                        )
                    }
                });
                Item {
                    title: entry.title.map(|title| title.content).unwrap_or_default(),
                    id: entry.id,
                    link: entry
                        .links
                        .into_iter()
                        .find(|link| {
                            link.target.is_none()
                                && matches!(link.rel.as_deref(), None | Some("" | "alternate"))
                        })
                        .map(|link| link.href)
                        .unwrap_or_default(),
                    content: content.unwrap_or_default(),
                }
            })
            .collect(),
    })
}

fn parse_json(input: &str) -> Result<Feed> {
    #[derive(Deserialize)]
    struct JsonFeed {
        title: Option<String>,
        items: Option<Vec<JsonItem>>,
    }
    #[derive(Deserialize)]
    struct JsonItem {
        id: Option<String>,
        title: Option<String>,
        url: Option<String>,
        content_html: Option<String>,
        content_text: Option<String>,
    }
    let feed: JsonFeed = serde_json::from_str(input)?;
    Ok(Feed {
        title: feed.title.unwrap_or_default(),
        items: feed
            .items
            .unwrap_or_default()
            .into_iter()
            .map(|item| Item {
                id: item.id.unwrap_or_default(),
                title: item.title.unwrap_or_default(),
                link: item.url.unwrap_or_default(),
                content: item
                    .content_html
                    .filter(|html| !html.is_empty())
                    .or(item.content_text)
                    .unwrap_or_default(),
            })
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::parse;

    #[test]
    fn keeps_legacy_guids_and_content_fields() {
        let rss = parse(br#"<rss xmlns:content="http://purl.org/rss/1.0/modules/content/" xmlns:atom="http://www.w3.org/2005/Atom"><channel>
            <title>News &amp; Notes</title>
            <item><title>A</title><link>https://a.test/1</link><atom:link href="https://wrong.test/"/><guid isPermaLink="false">post1</guid><content:encoded><![CDATA[<b>Body &amp;</b>]]></content:encoded></item>
            <item><link>https://a.test/2</link><description>Summary</description></item>
        </channel></rss>"#).unwrap();
        assert_eq!(rss.title, "News & Notes");
        assert_eq!(
            (
                &rss.items[0].id[..],
                &rss.items[0].link[..],
                &rss.items[0].content[..]
            ),
            ("post1", "https://a.test/1", "<b>Body &amp;</b>")
        );
        assert!(rss.items[1].id.is_empty() && rss.items[1].content.is_empty());

        let atom = parse(br#"<feed xmlns="http://www.w3.org/2005/Atom" xml:base="https://a.test/">
            <entry><id> tag:a.test,2020:entry </id><link rel="self" href="feed"/><link href="post"/><content type="xhtml"><div xmlns="http://www.w3.org/1999/xhtml"><p>Entry</p></div></content></entry>
            <entry><link href="missing-id"/></entry>
        </feed>"#).unwrap();
        assert_eq!(
            (
                &atom.items[0].id[..],
                &atom.items[0].link[..],
                &atom.items[0].content[..]
            ),
            (
                "tag:a.test,2020:entry",
                "https://a.test/post",
                "<p>Entry</p>"
            )
        );
        assert!(atom.items[1].id.is_empty());

        let json = parse(br#"{"title":"News","items":[{"id":"json:1","url":"https://a.test/1","content_text":"Plain text"}]}"#).unwrap();
        assert_eq!(
            (
                &json.items[0].id[..],
                &json.items[0].link[..],
                &json.items[0].content[..]
            ),
            ("json:1", "https://a.test/1", "Plain text")
        );
    }

    #[test]
    fn keeps_rdf_entries_with_guids_but_no_links() {
        let rdf = parse(br#"<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns="http://purl.org/rss/1.0/">
            <channel rdf:about="https://a.test/feed"><title>RDF feed</title></channel>
            <item rdf:about="https://a.test/entry"><guid>original&amp;id</guid></item>
        </rdf:RDF>"#).unwrap();
        assert_eq!(rdf.title, "RDF feed");
        assert_eq!(rdf.items[0].id, "original&id");
        assert!(rdf.items[0].link.is_empty());
    }
}

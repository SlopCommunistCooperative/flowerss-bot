use anyhow::Result;
use chrono::{DateTime, Utc};
use hard_xml::XmlRead;
use opml::{Body, Head, OPML};

use crate::store::Source;

// Legacy imports do not require version metadata or a nonempty body.
#[derive(XmlRead)]
#[xml(tag = "opml")]
struct Import {
    #[xml(child = "body")]
    body: Option<Body>,
}

pub(crate) struct Outline {
    pub text: String,
    pub url: String,
}

fn feed(outline: opml::Outline) -> Option<Outline> {
    Some(Outline {
        text: outline.text,
        url: outline.xml_url.filter(|url| !url.is_empty())?,
    })
}

pub(crate) fn read(bytes: &[u8]) -> Result<Vec<Outline>> {
    let document = Import::from_str(std::str::from_utf8(bytes)?)?;
    let mut feeds = Vec::new();
    for mut outline in document.body.unwrap_or_default().outlines {
        // The legacy importer reads root outlines and their immediate children only.
        feeds.extend(
            std::mem::take(&mut outline.outlines)
                .into_iter()
                .filter_map(feed),
        );
        feeds.extend(feed(outline));
    }
    Ok(feeds)
}

pub(crate) fn write<'a>(
    sources: impl Iterator<Item = &'a Source>,
    now: DateTime<Utc>,
) -> Result<String> {
    let document = OPML {
        head: Some(Head {
            title: Some("subscriptions in flowerss".into()),
            date_created: Some(now.format("%a, %d %b %Y %H:%M:%S UTC").to_string()),
            ..Head::default()
        }),
        body: Body {
            outlines: sources
                .map(|source| opml::Outline {
                    text: source.title.clone(),
                    r#type: Some("rss".into()),
                    xml_url: Some(source.link.clone()),
                    ..opml::Outline::default()
                })
                .collect(),
        },
        ..OPML::default()
    };
    Ok(format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n{}",
        document.to_string()?
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_legacy_order_and_depth() {
        let xml = br#"<opml version="2.0"><head/><body>
            <outline text="Folder" xmlUrl="https://parent.test/">
                <outline text="A &amp; B" xmlUrl="https://child.test/?a=1&amp;b=2"/>
                <outline text="Folder 2"><outline xmlUrl="https://ignored.test/"/></outline>
            </outline><outline text="no feed"/>
        </body></opml>"#;
        let feeds = read(xml).unwrap();
        assert_eq!(feeds.len(), 2);
        assert_eq!(feeds[0].text, "A & B");
        assert_eq!(feeds[0].url, "https://child.test/?a=1&b=2");
        assert_eq!(feeds[1].url, "https://parent.test/");
        assert!(read(b"<not-opml><body/></not-opml>").is_err());
    }

    #[test]
    fn accepts_versionless_and_empty_documents() {
        let feeds =
            read(br#"<opml><body><outline xmlUrl="https://a.test/"/></body></opml>"#).unwrap();
        assert_eq!(feeds[0].url, "https://a.test/");
        assert!(read(b"<opml/>").unwrap().is_empty());
    }
    #[test]
    fn export_round_trips_xml_attributes() {
        let source = Source {
            id: 1,
            title: "A & \"B\" <C>".into(),
            link: "https://example.com/?a=1&b=2".into(),
            error_count: 0,
        };
        let xml = write(std::iter::once(&source), DateTime::UNIX_EPOCH).unwrap();
        assert!(xml.contains("<opml version=\"2.0\">"));
        assert!(xml.contains("<title>subscriptions in flowerss</title>"));
        assert!(xml.contains("type=\"rss\""));
        let feeds = read(xml.as_bytes()).unwrap();
        assert_eq!(feeds[0].text, source.title);
        assert_eq!(feeds[0].url, source.link);
    }
}

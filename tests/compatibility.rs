use std::{
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use tiny_http::{Header, Response, Server};

const BIN: &str = env!("CARGO_BIN_EXE_flowerss-bot");

struct BotProcess(Child);
impl Drop for BotProcess {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn delivers_private_articles_as_rich_messages() -> Result<()> {
    exercise_delivery(private_chat())
}

#[test]
fn delivers_group_articles_as_rich_messages() -> Result<()> {
    exercise_delivery(
        json!({"id": -1000000000101_i64, "type": "supergroup", "title": "News"}),
    )
}

fn exercise_delivery(chat: Value) -> Result<()> {
    let chat_id = chat["id"].as_i64().unwrap();
    let dir = tempfile::tempdir()?;
    let server = Server::http("127.0.0.1:0").map_err(anyhow::Error::from_boxed)?;
    let endpoint = format!("http://{}", server.server_addr());
    let feed_url = format!("{endpoint}/feed");
    std::fs::write(
        dir.path().join("config.yml"),
        format!(
            "bot_token: test\ntelegram:\n  endpoint: {endpoint}\nupdate_interval: 0\npreview_text: 6\n",
        ),
    )?;
    let _bot = BotProcess(
        Command::new(BIN)
            .arg("-d")
            .arg(dir.path())
            .stdout(Stdio::null())
            .spawn()?,
    );

    let subscribe = format!("/sub {feed_url}");
    let extra_items = format!(
        "<item><title>Short</title><guid>short</guid><description><![CDATA[<p>Hi &amp; 🙂</p>]]></description></item>\
         <item><title>Oversized</title><guid>oversized</guid><description>{}尾</description></item>\
         <item><title>Many breaks</title><guid>many-breaks</guid><description><![CDATA[start{}end]]></description></item>",
        "长".repeat(30_000),
        "<br>".repeat(491),
    );
    let mut sent_command = false;
    let mut subscribed = false;
    let mut delivered = 0;
    let mut scans_after_delivery = 0;
    let deadline = Instant::now() + Duration::from_secs(8);
    while scans_after_delivery < 3 {
        let mut request = server
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))?
            .context("bot did not finish subscription and delivery")?;
        if request.url() == "/feed" {
            if delivered == 6 {
                scans_after_delivery += 1;
            }
            let new = if subscribed {
                "<item><title>New &amp; shiny</title><link>https://example.test/new</link><guid>new</guid><content:encoded><![CDATA[<b>新鲜</b><br>today]]></content:encoded></item>"
            } else {
                ""
            };
            request.respond(Response::from_string(format!(
                "<rss xmlns:content=\"http://purl.org/rss/1.0/modules/content/\"><channel><title>Feed</title><item><title>Old article</title><guid>old</guid></item>{new}{}</channel></rss>",
                if subscribed { extra_items.as_str() } else { "" },
            )))?;
            continue;
        }
        let mut body = String::new();
        request.as_reader().read_to_string(&mut body)?;
        let body: Value = serde_json::from_str(&body)?;
        let result = match request.url() {
            "/bottest/GetMe" => telegram_identity(),
            "/bottest/GetChatAdministrators" if chat_id < 0 => {
                assert_eq!(body["chat_id"], chat_id);
                json!([{"status": "creator", "user": telegram_user(), "is_anonymous": false}])
            }
            "/bottest/SetMyCommands" => {
                let commands = body["commands"].as_array().unwrap();
                assert!(commands.iter().any(|command| command["command"] == "sub"));
                assert!(
                    commands
                        .iter()
                        .any(|command| command["command"] == "export")
                );
                json!(true)
            }
            "/bottest/GetUpdates" if !sent_command => {
                assert_polling(&body, 1);
                sent_command = true;
                json!([{"update_id": 7, "message": {
                    "message_id": 19,
                    "date": 1,
                    "chat": chat,
                    "from": telegram_user(),
                    "text": subscribe,
                    "entities": [{"type": "bot_command", "offset": 0, "length": 4},
                                 {"type": "url", "offset": 5, "length": feed_url.len()}]
                }}])
            }
            "/bottest/GetUpdates" => {
                assert_polling(&body, 8);
                json!([])
            }
            "/bottest/SendMessage" if !subscribed => {
                assert_eq!(body["chat_id"], chat_id);
                let message = body["text"].as_str().unwrap();
                assert_eq!(message, format!("[[1]][Feed]({feed_url}) 订阅成功"));
                assert_eq!(body["parse_mode"], "Markdown");
                assert_eq!(body["reply_parameters"]["message_id"], 19);
                assert_eq!(body["link_preview_options"]["is_disabled"], true);
                subscribed = true;
                json!({"message_id": 20, "date": 1, "chat": chat, "text": message})
            }
            "/bottest/sendRichMessage" if subscribed => {
                assert_eq!(body["chat_id"], chat_id);
                assert!(body.get("parse_mode").is_none());
                assert_eq!(body["disable_notification"], matches!(delivered, 3 | 5));
                let html = body["rich_message"]["html"].as_str().unwrap();
                match delivered {
                    0 => {
                        assert_eq!(html, "<p><b>Feed</b></p><h3><a href=\"https://example.test/new\">New &amp; shiny</a></h3><details><summary>展开全文</summary><b>新鲜</b><br>today</details>");
                    }
                    1 => {
                        assert_eq!(html, "<p><b>Feed</b></p><h3>Short</h3><details><summary>展开全文</summary>\n<p>Hi &amp; 🙂</p>\n</details>");
                    }
                    2 => {
                        let first = "长".repeat(30_000);
                        assert_eq!(html, format!("<p><b>Feed</b></p><h3>Oversized</h3><details><summary>展开正文（第 1 部分）</summary><p>{first}</p></details>"));
                    }
                    3 => {
                        assert_eq!(html, "<p><b>Feed</b></p><h3>Oversized</h3><details><summary>展开正文（第 2 部分）</summary><p>尾</p></details>");
                    }
                    4 => {
                        let breaks = "<br>".repeat(489);
                        assert_eq!(html, format!("<p><b>Feed</b></p><h3>Many breaks</h3><details><summary>展开正文（第 1 部分）</summary><p>start{breaks}</p></details>"));
                    }
                    5 => {
                        assert_eq!(html, "<p><b>Feed</b></p><h3>Many breaks</h3><details><summary>展开正文（第 2 部分）</summary><p><br><br>end</p></details>");
                    }
                    _ => bail!("duplicate rich delivery"),
                }
                delivered += 1;
                json!({"message_id": 20 + delivered, "date": 1, "chat": chat, "rich_message": {"blocks": []}})
            }
            other => bail!("unexpected API request: {other}"),
        };
        respond_telegram(request, result)?;
    }
    Ok(())
}

fn telegram_identity() -> Value {
    json!({
        "id": 12, "is_bot": true, "first_name": "Flowerss", "username": "flowerss",
        "can_join_groups": true, "can_read_all_group_messages": false,
        "supports_inline_queries": false, "has_main_web_app": false
    })
}

fn private_chat() -> Value {
    json!({"id": 101, "type": "private", "first_name": "Tester"})
}

fn telegram_user() -> Value {
    json!({"id": 101, "is_bot": false, "first_name": "Tester"})
}

fn assert_polling(body: &Value, offset: i32) {
    assert_eq!(body["offset"], offset);
    assert_eq!(body["timeout"], 10);
    assert_eq!(
        body["allowed_updates"],
        json!(["message", "callback_query"])
    );
}

fn respond_telegram(request: tiny_http::Request, result: Value) -> Result<()> {
    request.respond(
        Response::from_string(json!({"ok": true, "result": result}).to_string())
            .with_header(Header::from_bytes("Content-Type", "application/json").unwrap()),
    )?;
    Ok(())
}

#[test]
fn reports_version_without_starting_bot() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let output = Command::new(BIN)
        .arg("-v")
        .current_dir(dir.path())
        .output()?;
    assert!(output.status.success());
    assert!(String::from_utf8(output.stdout)?.starts_with("version "));
    assert!(!dir.path().join("data.db").exists());
    Ok(())
}

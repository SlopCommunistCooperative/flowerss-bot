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
fn subscribes_and_delivers_only_new_feed_items() -> Result<()> {
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
    let mut sent_command = false;
    let mut subscribed = false;
    let mut delivered = false;
    let mut scans_after_delivery = 0;
    let deadline = Instant::now() + Duration::from_secs(8);
    while scans_after_delivery < 3 {
        let mut request = server
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))?
            .context("bot did not finish subscription and delivery")?;
        if request.url() == "/feed" {
            if delivered {
                scans_after_delivery += 1;
            }
            let new = if subscribed {
                "<item><title>New &amp; shiny</title><link>https://example.test/new</link><guid>new</guid><content:encoded><![CDATA[<b>新鲜</b><br>today]]></content:encoded></item>"
            } else {
                ""
            };
            request.respond(Response::from_string(format!(
                "<rss xmlns:content=\"http://purl.org/rss/1.0/modules/content/\"><channel><title>Feed</title><item><title>Old article</title><guid>old</guid></item>{new}</channel></rss>"
            )))?;
            continue;
        }
        let mut body = String::new();
        request.as_reader().read_to_string(&mut body)?;
        let body: Value = serde_json::from_str(&body)?;
        let result = match request.url() {
            "/bottest/GetMe" => telegram_identity(),
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
                    "chat": private_chat(),
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
            "/bottest/SendMessage" => {
                assert_eq!(body["chat_id"], 101);
                let message = body["text"].as_str().unwrap();
                if !subscribed {
                    assert_eq!(message, format!("[[1]][Feed]({feed_url}) 订阅成功"));
                    assert_eq!(body["parse_mode"], "Markdown");
                    assert_eq!(body["reply_parameters"]["message_id"], 19);
                    assert_eq!(body["link_preview_options"]["is_disabled"], true);
                    subscribed = true;
                } else {
                    assert!(!delivered, "duplicate delivery: {message}");
                    assert_eq!(
                        message,
                        "<b>Feed</b>\n---------- Preview ----------\n新鲜\ntod\n-----------------------------\n<a href=\"https://example.test/new\">New &amp; shiny</a>"
                    );
                    assert_eq!(body["parse_mode"], "HTML");
                    assert_eq!(body["disable_notification"], false);
                    assert!(body.get("reply_parameters").is_none());
                    delivered = true;
                }
                telegram_message(20, message)
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

fn telegram_message(message_id: i32, text: &str) -> Value {
    json!({"message_id": message_id, "date": 1, "chat": private_chat(), "text": text})
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
fn renders_configured_go_template_without_starting_bot() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let config = dir.path().join("custom.yml");
    std::fs::write(
        &config,
        "message_mode: Markdown\nmessage_tpl: '{{ if .EnableTelegraph }}{{.TelegraphURL}}{{else}}{{.PreviewText}}{{end}}'\n",
    )?;
    let output = Command::new(BIN)
        .arg("-v")
        .current_dir(dir.path())
        .output()?;
    assert!(output.status.success());
    assert!(String::from_utf8(output.stdout)?.starts_with("version "));
    let output = Command::new(BIN)
        .args(["-testtpl", "-c"])
        .arg(config)
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = String::from_utf8(output.stdout)?;
    assert!(output.contains("\\[1](123) <nil>"));
    assert!(output.contains("https://telegra.ph/markdown-07-07 <nil>"));
    assert!(!dir.path().join("data.db").exists());
    Ok(())
}

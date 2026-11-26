use anyhow::{Context, Result, ensure};
use reqwest::{Client, Url};
use teloxide_core::{
    ApiError, Bot, RequestError,
    requests::Requester,
    types::{FileId, LinkPreviewOptions, ParseMode},
};

// The legacy templates and command messages use Markdown, not MarkdownV2.
#[allow(deprecated)]
pub(crate) const MARKDOWN: ParseMode = ParseMode::Markdown;

pub(crate) fn connect(http: Client, endpoint: &str, token: &str) -> Result<Bot> {
    let endpoint =
        Url::parse(endpoint.trim_end_matches('/')).context("invalid Telegram endpoint")?;
    ensure!(
        matches!(endpoint.scheme(), "http" | "https"),
        "Telegram endpoint must be an HTTP(S) URL"
    );
    Ok(Bot::with_client(token, http).set_api_url(endpoint))
}

pub(crate) async fn download(bot: &Bot, id: FileId) -> Result<impl AsRef<[u8]>> {
    let file = bot.get_file(id).await?;
    let path = file.path.trim_start_matches('/');
    ensure!(!path.is_empty(), "Telegram file has no path");

    // teloxide-core 0.13 encodes the entire file path as one URL segment.
    // Preserve Telegram's directory separators for the actual file GET.
    let mut url = bot.api_url();
    url.path_segments_mut()
        .expect("Telegram endpoint is an HTTP(S) URL")
        .pop_if_empty()
        .extend(["file", &format!("bot{}", bot.token())])
        .extend(path.split('/'));
    let response = bot
        .client()
        .get(url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(reqwest::Error::without_url)?;
    Ok(response
        .bytes()
        .await
        .map_err(reqwest::Error::without_url)?)
}

pub(crate) fn disable_link_preview() -> LinkPreviewOptions {
    LinkPreviewOptions {
        is_disabled: true,
        url: None,
        prefer_small_media: false,
        prefer_large_media: false,
        show_above_text: false,
    }
}

pub(crate) fn is_forbidden(error: &RequestError) -> bool {
    match error {
        RequestError::Api(
            ApiError::BotBlocked
            | ApiError::BotKickedFromSupergroup
            | ApiError::BotKickedFromChannel
            | ApiError::UserDeactivated,
        ) => true,
        // The SDK discards the Bot API's error_code; unknown Telegram 403s
        // still carry the standard Forbidden description.
        RequestError::Api(ApiError::Unknown(description)) => {
            description == "Forbidden" || description.starts_with("Forbidden: ")
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_telegram_forbidden_errors() {
        for error in [
            ApiError::BotBlocked,
            ApiError::BotKickedFromSupergroup,
            ApiError::BotKickedFromChannel,
            ApiError::UserDeactivated,
            ApiError::Unknown("Forbidden: bot is not a member of the channel chat".into()),
        ] {
            assert!(is_forbidden(&RequestError::Api(error)));
        }
        assert!(!is_forbidden(&RequestError::Api(ApiError::ChatNotFound)));
    }
}

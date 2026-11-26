mod app;
mod article;
mod bot;
mod config;
mod feed;
mod opml;
mod store;
mod telegram;

use std::{fs::OpenOptions, sync::Mutex};

use anyhow::{Context, Result};
use tracing::level_filters::LevelFilter;

pub fn version() -> String {
    format!(
        "version {}, commit {}, built at {}",
        option_env!("APP_VERSION").unwrap_or("dev"),
        option_env!("APP_COMMIT").unwrap_or("none"),
        option_env!("BUILD_TIME").unwrap_or("unknown"),
    )
}

fn usage() {
    let program = std::env::args_os()
        .next()
        .unwrap_or_else(|| "flowerss-bot".into());
    eprintln!(
        "Usage of {}:\n  -c string\n    \tconfig file of flowerss\n  -d string\n    \twork directory of flowerss (default \"./\")\n  -v\n    \tprints flowerss-bot version",
        program.to_string_lossy()
    );
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let cli = match config::Cli::parse(std::env::args_os().skip(1)) {
        Ok(cli) => cli,
        Err(err) => {
            eprintln!("{err}");
            usage();
            return std::process::ExitCode::from(2);
        }
    };
    if cli.help {
        usage();
    } else if cli.version {
        print!("{}", version());
    } else if let Err(err) = run(cli).await {
        eprintln!("{err:#}");
        return std::process::ExitCode::FAILURE;
    }
    std::process::ExitCode::SUCCESS
}

async fn run(cli: config::Cli) -> Result<()> {
    let config = config::Config::load(&cli.config, &cli.work_dir)?;
    init_logging(&config.log)?;
    let app = app::App::open(config).await?;
    let telegram = telegram::connect(
        app.http.clone(),
        &app.config.telegram_endpoint,
        &app.config.bot_token,
    )?;
    let polling = telegram::connect(
        app.telegram_poll_http.clone(),
        &app.config.telegram_endpoint,
        &app.config.bot_token,
    )?;
    tracing::info!(version = %version(), "bot start");
    tokio::select! {
        result = bot::run(&app, &telegram, &polling) => result,
        () = app.schedule(&telegram) => Ok(()),
        result = shutdown() => result,
    }
}

fn init_logging(log: &config::Log) -> Result<()> {
    let level = if log.level.eq_ignore_ascii_case("debug") {
        LevelFilter::DEBUG
    } else {
        LevelFilter::INFO
    };
    let config = tracing_subscriber::fmt().with_max_level(level);
    if log.file.is_empty() {
        config.with_writer(std::io::stderr).init();
    } else {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log.file)
            .with_context(|| format!("open log file {}", log.file))?;
        config.json().with_writer(Mutex::new(file)).init();
    }
    Ok(())
}

async fn shutdown() -> Result<()> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut terminate = signal(SignalKind::terminate())?;
        let mut quit = signal(SignalKind::quit())?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result?,
            _ = terminate.recv() => (),
            _ = quit.recv() => (),
        }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await?;
    Ok(())
}

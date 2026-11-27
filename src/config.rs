use std::{
    borrow::Cow,
    ffi::OsString,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde_json::Value;

const TELEGRAM_ENDPOINT: &str = "https://api.telegram.org";

pub struct Config {
    pub bot_token: String,
    pub socks5: String,
    pub user_agent: String,
    pub update_interval: i64,
    pub error_threshold: i64,
    pub allowed_users: Vec<i64>,
    pub telegram_endpoint: String,
    pub sqlite: Sqlite,
    pub mysql: Mysql,
    pub log: Log,
}

pub struct Sqlite {
    pub path: PathBuf,
}

pub struct Mysql {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: String,
    pub database: String,
}

pub struct Log {
    pub level: String,
    pub file: String,
    pub db_log: bool,
}

impl Config {
    pub fn load(path: &Path, work_dir: &Path) -> Result<Self> {
        let input = std::fs::read_to_string(path)
            .with_context(|| format!("Fatal error config file: {}", path.display()))?;
        let settings: Value = match path.extension().and_then(|ext| ext.to_str()) {
            Some("json") => serde_json::from_str(&input)?,
            Some("toml") => toml::from_str(&input)?,
            _ => serde_saphyr::from_str(&input)?,
        };
        let get = |path: &str| {
            path.split('.')
                .try_fold(&settings, |node, key| {
                    node.as_object()?
                        .iter()
                        .find_map(|(name, value)| name.eq_ignore_ascii_case(key).then_some(value))
                })
                .filter(|value| !value.is_null())
        };
        let string = |key| get(key).map(text).unwrap_or_default().into_owned();
        let int = |key, default| {
            get(key).map_or(default, |value| {
                value
                    .as_i64()
                    .or_else(|| text(value).parse().ok())
                    .unwrap_or_default()
            })
        };
        let boolean = |key| {
            get(key).is_some_and(|value| {
                matches!(
                    text(value).to_ascii_lowercase().as_str(),
                    "true" | "t" | "1"
                )
            })
        };
        let list = |key| match get(key) {
            Some(Value::Array(values)) => values
                .iter()
                .map(|value| text(value).into_owned())
                .collect(),
            Some(Value::String(value)) => value.split_whitespace().map(str::to_owned).collect(),
            Some(value) => vec![text(value).into_owned()],
            None => Vec::new(),
        };
        let allowed_users = list("allowed_users")
            .into_iter()
            .map(|value| value.parse::<i64>().context("invalid allowed_users entry"))
            .collect::<Result<_>>()?;
        Ok(Self {
            bot_token: string("bot_token"),
            socks5: string("socks5"),
            user_agent: string("user_agent"),
            update_interval: int("update_interval", 10),
            error_threshold: int("error_threshold", 100),
            allowed_users,
            telegram_endpoint: get("telegram.endpoint")
                .map_or_else(|| TELEGRAM_ENDPOINT.into(), |v| text(v).into_owned()),
            sqlite: Sqlite {
                path: get("sqlite.path").map_or_else(
                    || work_dir.join("data.db"),
                    |v| PathBuf::from(text(v).as_ref()),
                ),
            },
            mysql: Mysql {
                host: string("mysql.host"),
                port: u16::try_from(int("mysql.port", 0)).context("invalid mysql.port")?,
                user: string("mysql.user"),
                password: string("mysql.password"),
                database: string("mysql.database"),
            },
            log: Log {
                level: string("log.level"),
                file: string("log.file"),
                db_log: boolean("log.db_log"),
            },
        })
    }
}

fn text(value: &Value) -> Cow<'_, str> {
    match value {
        Value::String(text) => Cow::Borrowed(text),
        Value::Number(_) | Value::Bool(_) => Cow::Owned(value.to_string()),
        _ => Cow::Borrowed(""),
    }
}

pub struct Cli {
    pub work_dir: PathBuf,
    pub config: PathBuf,
    pub version: bool,
    pub help: bool,
}

impl Cli {
    pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Self> {
        let mut args = args.into_iter();
        let mut work_dir = PathBuf::from(".");
        let mut config = PathBuf::new();
        let mut version = false;
        let mut help = false;
        while let Some(arg) = args.next() {
            let arg = arg.to_string_lossy();
            let Some(opt) = arg.strip_prefix('-') else {
                break;
            };
            let opt = opt.strip_prefix('-').unwrap_or(opt);
            let (name, value) = opt
                .split_once('=')
                .map_or((opt, None), |(n, v)| (n, Some(v)));
            match name {
                "" if arg == "--" || arg == "-" => break,
                "d" | "c" => {
                    let value = value
                        .map(OsString::from)
                        .or_else(|| args.next())
                        .with_context(|| format!("flag needs an argument: -{name}"))?;
                    if name == "d" {
                        work_dir = value.into()
                    } else {
                        config = value.into()
                    }
                }
                "v" => {
                    version = match value {
                        None | Some("1" | "t" | "T" | "true" | "TRUE" | "True") => true,
                        Some("0" | "f" | "F" | "false" | "FALSE" | "False") => false,
                        Some(value) => bail!("invalid boolean value {value:?} for -{name}"),
                    };
                }
                "h" | "help" => {
                    help = true;
                    break;
                }
                _ => bail!("flag provided but not defined: -{name}"),
            }
        }
        if config.as_os_str().is_empty() {
            config = work_dir.join("config.yml");
        }
        Ok(Self {
            work_dir,
            config,
            version,
            help,
        })
    }
}

//! Command-line entry point; terminal integration is layered over the same agent.

use clap::{Parser, Subcommand};
use gitcode::{
    agent::{Agent, BASE_PROMPT},
    config::{self, Overrides},
    credentials::{self, Secret},
    headless::{generate_completions, run_doctor, HeadlessEvent, OutputFormat},
    logging::Logger,
    mcp::{add_mcp_server, list_mcp_servers, remove_mcp_server, McpServerConfig},
    perm::{Engine, Mode},
    provider::{
        canonical::{Message, Role},
        transport::Client,
    },
    theme::ColorMode,
    tools::Tools,
    usage::{render_cost_short, render_usage_panel, UsageLedger, UsageSummary},
    web_git::GitRepo,
};
use std::{collections::BTreeMap, io::IsTerminal, path::PathBuf, time::Duration};

#[derive(Parser)]
#[command(name = "gitcode", version, about = "Local-first terminal coding agent")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
    #[arg(short = 'p', long = "print")]
    prompt: Option<String>,
    #[arg(long)]
    model: Option<String>,
    #[arg(long)]
    provider: Option<String>,
    #[arg(long)]
    base_url: Option<String>,
    #[arg(long, value_parser = ["anthropic", "openai-chat", "openai-responses"])]
    protocol: Option<String>,
    #[arg(long, default_value = "text", value_parser = ["text", "json", "stream-json"])]
    output_format: String,
    #[arg(long, default_value = "text", value_parser = ["text", "stream-json"])]
    input_format: String,
    #[arg(long)]
    max_turns: Option<u32>,
    #[arg(long)]
    max_budget_usd: Option<f64>,
    #[arg(long)]
    dangerously_skip_permissions: bool,
    #[arg(long)]
    system_prompt: Option<String>,
    #[arg(long)]
    append_system_prompt: Option<String>,
    #[arg(long = "continue")]
    continue_session: bool,
    #[arg(long)]
    resume: Option<String>,
    #[arg(long = "add-dir")]
    add_dirs: Vec<PathBuf>,
    #[arg(long)]
    verbose: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Execute a single non-interactive prompt (Codex CLI style).
    Exec {
        /// Prompt string to execute.
        prompt: String,
    },
    /// Run diagnostic checks on git, workspace, configuration, and credentials.
    Doctor {
        /// Emit structured JSON report.
        #[arg(long)]
        json: bool,
    },
    /// Generate shell completion script (`bash`, `zsh`, or `fish`).
    Completions {
        /// Target shell name.
        shell: String,
    },
    /// Inspect persisted token and cost ledger.
    Usage {
        /// Emit JSON summary.
        #[arg(long)]
        json: bool,
        /// Optional minimum timestamp in Unix milliseconds.
        #[arg(long)]
        since: Option<u64>,
    },
    /// Manage `.mcp.json` server entries (`list`, `add`, `remove`).
    Mcp {
        /// Action (`list`, `add`, or `remove`).
        action: String,
        /// Server name for `add` or `remove`.
        name: Option<String>,
        /// Command or URL for `add`.
        target: Option<String>,
    },
}

#[tokio::main]
async fn main() {
    if let Err(error) = logged_run().await {
        eprintln!("Ошибка: {error}");
        std::process::exit(1);
    }
}

async fn logged_run() -> Result<(), String> {
    // Parse first so help/version never create application state.
    let cli = Cli::parse();
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or("не задан HOME")?;
    let mut logger = tokio::task::spawn_blocking(move || {
        let directory = home.join(".gitcode");
        std::fs::create_dir_all(&directory).map_err(|_| "не удалось создать каталог журнала")?;
        let metadata = std::fs::symlink_metadata(&directory)
            .map_err(|_| "не удалось проверить каталог журнала")?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err("каталог журнала должен быть обычным каталогом".to_owned());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))
                .map_err(|_| "не удалось защитить каталог журнала")?;
        }
        let secrets = std::env::vars()
            .filter(|(key, _)| {
                let key = key.to_ascii_uppercase();
                key.contains("KEY")
                    || key.contains("TOKEN")
                    || key.contains("SECRET")
                    || key.contains("PASSWORD")
            })
            .map(|(_, value)| Secret::new(value))
            .collect();
        let mut logger = Logger::open(&directory.join("gitcode.log"), secrets)
            .map_err(|_| "не удалось открыть журнал")?;
        logger
            .record("info", "application started")
            .map_err(|_| "не удалось записать журнал")?;
        Ok::<_, String>(logger)
    })
    .await
    .map_err(|_| "не удалось запустить журнал")??;
    let result = run(cli, &mut logger).await;
    tokio::task::spawn_blocking(move || {
        let result = result.map_err(|message| logger.redact(&message));
        let (level, message) = match &result {
            Ok(()) => ("info", "application finished"),
            Err(message) => ("error", message.as_str()),
        };
        logger
            .record(level, message)
            .map_err(|_| "не удалось записать журнал")?;
        result
    })
    .await
    .map_err(|_| "не удалось завершить журнал")?
}

async fn run(cli: Cli, logger: &mut Logger) -> Result<(), String> {
    let root = std::env::current_dir().map_err(|_| "не удалось определить рабочий каталог")?;
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or("не задан HOME")?;
    let env: BTreeMap<String, String> = std::env::vars().collect();
    let mut overrides = Overrides::new();
    if let Some(model) = cli.model.clone() {
        overrides.insert("model".into(), toml::Value::String(model));
    }
    if let Some(limit) = cli.max_turns {
        overrides.insert("max_turns".into(), toml::Value::Integer(i64::from(limit)));
    }
    let config_home = home.clone();
    let config_root = root.clone();
    let config_env = env.clone();
    let config_res = tokio::task::spawn_blocking(move || {
        config::load(&config_home, &config_root, &config_env, &overrides)
    })
    .await
    .map_err(|_| "не удалось загрузить конфигурацию")?;
    let secret = if let Ok(cfg) = &config_res {
        let home = home.clone();
        let key_env = cfg.provider.api_key_env.clone();
        let env = env.clone();
        tokio::task::spawn_blocking(move || credentials::resolve(&home, &key_env, &env))
            .await
            .map_err(|_| "не удалось загрузить ключ провайдера")?
            .map_err(|_| "не удалось прочитать приватный ключ провайдера")?
    } else {
        None
    };
    if let Some(secret) = &secret {
        logger.add_secret(Secret::new(secret.expose().to_owned()));
    }

    if let Some(cmd) = &cli.command {
        match cmd {
            Commands::Completions { shell } => {
                let script = generate_completions(shell)?;
                print!("{script}");
                return Ok(());
            }
            Commands::Doctor { json } => {
                let (cfg_ok, has_key) = match &config_res {
                    Ok(cfg) => (
                        cfg.validate(std::path::Path::new("CLI")).is_ok(),
                        secret.is_some(),
                    ),
                    Err(_) => (false, false),
                };
                let report = run_doctor(&root, cfg_ok, has_key);
                if *json {
                    println!(
                        "{}",
                        serde_json::to_string(&report).map_err(|e| e.to_string())?
                    );
                } else {
                    for check in &report.checks {
                        let mark = if check.ok { "ok" } else { "fail" };
                        println!("[{mark}] {}: {}", check.name, check.detail);
                    }
                }
                return Ok(());
            }
            Commands::Usage { json, since } => {
                let ledger = UsageLedger::open(&home).map_err(|e| e.to_string())?;
                let entries = ledger.entries(*since).map_err(|e| e.to_string())?;
                if *json {
                    let summary = UsageSummary::from_entries(&entries);
                    println!(
                        "{}",
                        serde_json::to_string(&summary).map_err(|e| e.to_string())?
                    );
                } else {
                    let lines = render_usage_panel(&entries, "current", 0, ColorMode::None);
                    for line in lines {
                        println!("{line}");
                    }
                    println!(
                        "{}",
                        render_cost_short(&UsageSummary::from_entries(&entries))
                    );
                }
                return Ok(());
            }
            Commands::Mcp {
                action,
                name,
                target,
            } => {
                let mcp_path = root.join(".mcp.json");
                match action.as_str() {
                    "list" => {
                        for (srv, cfg) in list_mcp_servers(&mcp_path)? {
                            println!("{srv}: {cfg:?}");
                        }
                    }
                    "add" => {
                        let srv = name.as_deref().ok_or("укажите имя MCP-сервера")?;
                        let tgt = target.as_deref().ok_or("укажите команду или URL")?;
                        let cfg = if tgt.starts_with("http://") || tgt.starts_with("https://") {
                            McpServerConfig::Http {
                                url: tgt.to_owned(),
                                headers: BTreeMap::new(),
                            }
                        } else {
                            McpServerConfig::Stdio {
                                command: tgt.to_owned(),
                                args: Vec::new(),
                                env: BTreeMap::new(),
                            }
                        };
                        add_mcp_server(&mcp_path, srv, cfg)?;
                    }
                    "remove" => {
                        let srv = name.as_deref().ok_or("укажите имя MCP-сервера")?;
                        remove_mcp_server(&mcp_path, srv)?;
                    }
                    other => return Err(format!("неизвестное действие mcp `{other}`")),
                }
                return Ok(());
            }
            Commands::Exec { .. } => {}
        }
    }

    let mut config = config_res.map_err(|e| e.to_string())?;
    if let Some(url) = cli.base_url {
        config.provider.base_url = url;
    }
    if let Some(protocol) = cli.protocol {
        config.provider.protocol = match protocol.as_str() {
            "openai-chat" => config::Protocol::OpenaiChat,
            "openai-responses" => config::Protocol::OpenaiResponses,
            _ => config::Protocol::Anthropic,
        };
    }
    let prompt = match cli.command {
        Some(Commands::Exec { prompt }) => Some(prompt),
        _ => cli.prompt,
    };
    config
        .validate(std::path::Path::new("CLI"))
        .map_err(|e| e.to_string())?;
    let bearer = config.provider.protocol != config::Protocol::Anthropic;
    let client = Client::with_protocol(
        &config.provider.base_url,
        secret,
        bearer,
        Duration::from_secs(config.provider.timeout_seconds),
        config.provider.protocol,
    )?;
    let mode = if cli.dangerously_skip_permissions {
        Mode::Bypass
    } else {
        Mode::Default
    };
    let tools = Tools::new(Engine::new(&root, mode)?);
    let mut system = cli.system_prompt.unwrap_or_else(|| BASE_PROMPT.into());
    if let Some(append) = cli.append_system_prompt {
        system.push('\n');
        system.push_str(&append);
    }
    system.push_str(&format!(
        "\nWorkspace: {}. Platform: {}. Language setting: {}.",
        root.display(),
        std::env::consts::OS,
        config.language
    ));
    let session_home = home.clone();
    let session_root = root.clone();
    let resume_id = cli.resume.clone();
    let continue_session = cli.continue_session;
    let (store, session) = tokio::task::spawn_blocking(move || {
        let store = gitcode::session::store::SessionStore::open(&session_home, &session_root)?;
        let id = if let Some(id) = resume_id {
            Some(id)
        } else if continue_session {
            Some(
                store
                    .list()?
                    .first()
                    .ok_or_else(|| {
                        std::io::Error::new(std::io::ErrorKind::NotFound, "no session to continue")
                    })?
                    .id
                    .clone(),
            )
        } else {
            None
        };
        let session = match id {
            Some(id) => store.resume(&id)?,
            None => store.create("gitcode session", None)?,
        };
        Ok::<_, std::io::Error>((store, session))
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())?;
    let memory_home = home.clone();
    let memory_root = root.clone();
    let memory = tokio::task::spawn_blocking(move || {
        gitcode::session::memory::load(&memory_home, &memory_root, &memory_root)
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())?;
    system.push('\n');
    system.push_str(&memory.text);
    let mut agent = Agent {
        client,
        tools,
        model: config.model.clone(),
        system,
        max_turns: config.max_turns,
        max_tokens: 8192,
    };
    if let Some(prompt) = prompt {
        let format = OutputFormat::parse(&cli.output_format)?;
        if format == OutputFormat::StreamJson {
            let init = HeadlessEvent::Init {
                session_id: "headless".into(),
                model: config.model.clone(),
                provider: config.provider.name.clone(),
                tools: Tools::definitions().into_iter().map(|d| d.name).collect(),
            };
            println!("{}", init.to_jsonl()?);
        }
        let committed = session.messages.len();
        let mut history = session.messages.clone();
        history.push(Message::text(Role::User, prompt));
        let outcome = agent.run(history, None).await?;
        let id = session.id.clone();
        let appended = outcome.messages[committed..].to_vec();
        tokio::task::spawn_blocking(move || {
            for message in appended {
                store.append(&id, &message)?;
            }
            Ok::<_, std::io::Error>(())
        })
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
        match format {
            OutputFormat::Text => println!("{}", outcome.text),
            OutputFormat::Json | OutputFormat::StreamJson => {
                let res = HeadlessEvent::Result {
                    text: outcome.text,
                    usage: outcome.usage,
                    requests: outcome.requests as usize,
                    duration_ms: 0,
                    is_error: false,
                };
                println!("{}", res.to_jsonl()?);
            }
        }
    } else {
        if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
            return Err("интерактивный режим требует терминал; используйте -p \"запрос\"".into());
        }
        let cwd = root.display().to_string();
        let permission = format!("{mode:?}");
        let branch = GitRepo::discover(&root)
            .and_then(|r| r.status_summary().ok())
            .map(|s| {
                if s.dirty {
                    format!("{}*", s.branch)
                } else {
                    s.branch
                }
            })
            .unwrap_or_else(|| "main".into());
        gitcode::tui::app::run(
            &mut agent,
            gitcode::tui::app::Options {
                model: &config.model,
                provider: &config.provider.name,
                cwd: &cwd,
                branch: &branch,
                permission: &permission,
                no_intro: !config.intro,
                session: Some((store, session)),
            },
        )
        .await
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

//! Command-line entry point; terminal integration is layered over the same agent.

use clap::Parser;
use gitcode::{
    agent::{Agent, BASE_PROMPT},
    config::{self, Overrides},
    credentials::Secret,
    perm::{Engine, Mode},
    provider::{
        canonical::{Message, Role},
        transport::Client,
    },
    tools::Tools,
};
use std::{collections::BTreeMap, io::IsTerminal, path::PathBuf, time::Duration};

#[derive(Parser)]
#[command(name = "gitcode", version, about = "Local-first terminal coding agent")]
struct Cli {
    #[arg(short = 'p', long = "print")]
    prompt: Option<String>,
    #[arg(long)]
    model: Option<String>,
    #[arg(long)]
    base_url: Option<String>,
    #[arg(long,value_parser=["anthropic","openai-chat","openai-responses"])]
    protocol: Option<String>,
    #[arg(long)]
    max_turns: Option<u32>,
    #[arg(long)]
    dangerously_skip_permissions: bool,
    #[arg(long)]
    system_prompt: Option<String>,
    #[arg(long)]
    append_system_prompt: Option<String>,
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("Ошибка: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    let cli = Cli::parse();
    let root = std::env::current_dir().map_err(|_| "не удалось определить рабочий каталог")?;
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or("не задан HOME")?;
    let env: BTreeMap<String, String> = std::env::vars().collect();
    let mut overrides = Overrides::new();
    if let Some(model) = cli.model {
        overrides.insert("model".into(), toml::Value::String(model));
    }
    if let Some(limit) = cli.max_turns {
        overrides.insert("max_turns".into(), toml::Value::Integer(i64::from(limit)));
    }
    let mut config = config::load(&home, &root, &env, &overrides).map_err(|e| e.to_string())?;
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
    let prompt = cli.prompt;
    config
        .validate(std::path::Path::new("CLI"))
        .map_err(|e| e.to_string())?;
    let secret = env
        .get(&config.provider.api_key_env)
        .filter(|s| !s.is_empty())
        .map(|s| Secret::new(s.clone()));
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
    let mut agent = Agent {
        client,
        tools,
        model: config.model.clone(),
        system,
        max_turns: config.max_turns,
        max_tokens: 8192,
    };
    if let Some(prompt) = prompt {
        let outcome = agent
            .run(vec![Message::text(Role::User, prompt)], None)
            .await?;
        println!("{}", outcome.text);
    } else {
        if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
            return Err("интерактивный режим требует терминал; используйте -p \"запрос\"".into());
        }
        let cwd = root.display().to_string();
        let permission = format!("{mode:?}");
        gitcode::tui::app::run(
            &mut agent,
            gitcode::tui::app::Options {
                model: &config.model,
                provider: &config.provider.name,
                cwd: &cwd,
                branch: "unknown",
                permission: &permission,
                no_intro: !config.intro,
            },
        )
        .await
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

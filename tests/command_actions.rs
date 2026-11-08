//! Offline command behavior checks with an isolated home and workspace.
#![allow(clippy::unwrap_used)]

use gitcode::{
    agent::Agent,
    perm::{Engine, Mode, Rule},
    provider::{
        canonical::{Message, Role},
        transport::Client,
    },
    session::store::SessionStore,
    tools::Tools,
    tui::{commands::execute_command, statusbar::Status},
};
use std::{
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use unicode_width::UnicodeWidthStr;

struct Sandbox {
    root: PathBuf,
    previous_home: Option<std::ffi::OsString>,
    previous_profile: Option<std::ffi::OsString>,
}
impl Sandbox {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "gitcode-command-actions-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("home")).unwrap();
        std::fs::create_dir_all(root.join("workspace")).unwrap();
        let root = root.canonicalize().unwrap();
        let previous_home = std::env::var_os("HOME");
        let previous_profile = std::env::var_os("USERPROFILE");
        std::env::set_var("HOME", root.join("home"));
        std::env::set_var("USERPROFILE", root.join("home"));
        Self {
            root,
            previous_home,
            previous_profile,
        }
    }
}
impl Drop for Sandbox {
    fn drop(&mut self) {
        if let Some(home) = &self.previous_home {
            std::env::set_var("HOME", home);
        } else {
            std::env::remove_var("HOME");
        }
        if let Some(profile) = &self.previous_profile {
            std::env::set_var("USERPROFILE", profile);
        } else {
            std::env::remove_var("USERPROFILE");
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

async fn run(
    command: &str,
    agent: &mut Agent,
    messages: &mut Vec<Message>,
    status: &mut Status,
) -> String {
    execute_command(command, agent, messages, status, 160)
        .await
        .unwrap()
        .unwrap()
        .join("\n")
}

// One test owns HOME for the entire integration-test process; no parallel env mutation.
#[tokio::test]
async fn command_actions_are_honest_permission_checked_and_isolated() {
    let sandbox = Sandbox::new();
    let workspace = sandbox.root.join("workspace");
    let home = sandbox.root.join("home");
    let mut agent = Agent {
        client: Client::new(
            "http://127.0.0.1:9",
            None,
            false,
            Duration::from_millis(100),
        )
        .unwrap(),
        tools: Tools::new(Engine::new(&workspace, Mode::Default).unwrap()),
        model: "offline-test".into(),
        system: String::new(),
        max_turns: 4,
        max_tokens: 128,
    };
    let mut status = Status::default();
    let mut messages = vec![Message::text(Role::Assistant, "Ответ для проверки")];
    let copy = run("/copy", &mut agent, &mut messages, &mut status).await;
    assert!(copy.contains("Копирование не выполнено"));
    assert!(status.toast.is_none());

    for mode in [Mode::Default, Mode::Plan] {
        agent.tools.permissions.mode = mode;
        let init = run("/init", &mut agent, &mut messages, &mut status).await;
        assert!(init.contains("не создан"));
        assert!(!workspace.join("GITCODE.md").exists());
    }
    agent.tools.permissions.mode = Mode::Bypass;
    agent
        .tools
        .permissions
        .deny
        .push(Rule::parse("Write(*)").unwrap());
    assert!(run("/init", &mut agent, &mut messages, &mut status)
        .await
        .contains("не создан"));
    assert!(!workspace.join("GITCODE.md").exists());
    agent.tools.permissions.deny.clear();
    agent.tools.permissions.mode = Mode::AcceptEdits;
    assert!(run("/init", &mut agent, &mut messages, &mut status)
        .await
        .contains("Создан шаблон"));
    let template = std::fs::read(workspace.join("GITCODE.md")).unwrap();
    run("/init", &mut agent, &mut messages, &mut status).await;
    assert_eq!(
        std::fs::read(workspace.join("GITCODE.md")).unwrap(),
        template
    );

    let store = SessionStore::open(&home, &workspace).unwrap();
    let session = store
        .create("Saved context", Some("saved-branch".into()))
        .unwrap();
    store
        .append(&session.id, &Message::text(Role::User, "Saved question"))
        .unwrap();
    let restored = run(
        &format!("/resume {}", session.id),
        &mut agent,
        &mut messages,
        &mut status,
    )
    .await;
    assert!(restored.contains("Контекст загружен"));
    assert!(restored.contains("активный журнал сохранения"));
    assert_eq!(
        serde_json::to_value(&messages).unwrap(),
        serde_json::to_value(store.resume(&session.id).unwrap().messages).unwrap()
    );
    let before = serde_json::to_value(&messages).unwrap();
    for invalid in ["not-a-session", "../../escape"] {
        assert!(run(
            &format!("/resume {invalid}"),
            &mut agent,
            &mut messages,
            &mut status
        )
        .await
        .contains("не изменён"));
        assert_eq!(serde_json::to_value(&messages).unwrap(), before);
    }
    assert!(run("/commit draft", &mut agent, &mut messages, &mut status)
        .await
        .contains("Коммит не создаётся"));
    assert!(run("/vim", &mut agent, &mut messages, &mut status)
        .await
        .contains("Vim не реализован"));
    assert!(
        run("/theme unsupported", &mut agent, &mut messages, &mut status)
            .await
            .contains("не поддерживает аргументы")
    );
    assert!(run(
        "/config max_turns invalid",
        &mut agent,
        &mut messages,
        &mut status
    )
    .await
    .contains("не изменено"));
    assert_eq!(agent.max_turns, 4);
    run(
        "/permissions ask Write(*)",
        &mut agent,
        &mut messages,
        &mut status,
    )
    .await;
    assert_eq!(agent.tools.permissions.ask.len(), 1);
    assert!(run(
        "/export denied.json",
        &mut agent,
        &mut messages,
        &mut status
    )
    .await
    .contains("отклонено"));
    assert!(!workspace.join("denied.json").exists());
    agent.tools.permissions.ask.clear();
    assert!(run(
        "/export conversation.json",
        &mut agent,
        &mut messages,
        &mut status
    )
    .await
    .contains("успешно сохранён"));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(
            &std::fs::read(workspace.join("conversation.json")).unwrap()
        )
        .unwrap(),
        before
    );

    for width in [60, 80, 120] {
        for command in gitcode::extensibility::BUILTIN_COMMANDS {
            let card = execute_command(
                &format!("/{}", command.name),
                &mut agent,
                &mut messages,
                &mut status,
                width,
            )
            .await
            .unwrap()
            .unwrap();
            assert!(card.len() >= 3, "/{}", command.name);
            assert!(
                card.iter().all(|line| line.width() <= width),
                "/{} at {width}",
                command.name
            );
        }
    }
}

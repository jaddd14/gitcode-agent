//! Local command presentation, honest capability descriptions, and structured command cards.

use super::{
    statusbar::{compact, Status},
    usage_panel,
    widgets::{self, Glyphs, Table},
};
use crate::{
    agent::{agency::load_agents, context, Agent},
    extensibility::{complete_slash, discover_skills, SlashCommandInfo, BUILTIN_COMMANDS},
    headless::run_doctor,
    mcp::{list_mcp_servers, McpServerConfig},
    perm::{Mode, Rule},
    provider::canonical::{Content, Message, Role},
    session::store::SessionStore,
    usage::UsageLedger,
};
use std::{
    io,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

/// Russian command descriptions shared by help, completion, and command cards.
pub fn description(name: &str) -> &'static str {
    match name {
        "help" => "Команды и горячие клавиши",
        "clear" | "new" => "Очистить контекст разговора",
        "compact" => "Сократить результаты инструментов",
        "config" => "Параметры конфигурации",
        "model" => "Выбрать модель",
        "provider" => "Настройки провайдера",
        "usage" => "Использование токенов по периодам",
        "cost" => "Стоимость текущей сессии",
        "status" => "Состояние сессии и проекта",
        "context" => "Состав контекста",
        "resume" => "Сохранённые сессии",
        "rewind" => "Контрольные точки",
        "plan" => "Режим планирования",
        "permissions" => "Режим и правила доступа",
        "agents" => "Определения агентов",
        "mcp" => "Серверы инструментов",
        "init" => "Инструкции проекта",
        "review" => "Сводка состояния git (без анализа кода)",
        "diff" => "Изменения файлов",
        "commit" => "Проверить готовность к коммиту (без записи)",
        "memory" => "Память проекта",
        "doctor" => "Диагностика окружения",
        "theme" => "Палитра интерфейса",
        "vim" => "Режим редактирования",
        "export" => "Сохранить разговор в файл",
        "copy" => "Копировать последний ответ",
        "hooks" => "Обработчики событий",
        "skills" => "Доступные навыки",
        "add-dir" => "Дополнительные папки",
        "login" => "Настройка авторизации",
        "logout" => "Управление сохранённым ключом",
        "terminal-setup" => "Клавиши терминала",
        "exit" => "Закрыть приложение",
        _ => "Своя команда",
    }
}

/// Semantic category used as a redundant textual chip.
pub fn category(name: &str) -> &'static str {
    match name {
        "model" | "provider" => "модель",
        "context" | "compact" | "memory" | "add-dir" => "контекст",
        "diff" | "commit" | "review" => "git",
        "agents" | "mcp" | "skills" | "hooks" => "инструменты",
        "help" | "config" | "doctor" | "theme" | "vim" | "login" | "logout" | "terminal-setup" => {
            "система"
        }
        _ => "сессия",
    }
}

/// Synchronous in-memory candidates; never performs IO on a keystroke.
pub fn candidates(text: &str) -> Vec<SlashCommandInfo> {
    complete_slash(text.split_whitespace().next().unwrap_or("/"))
        .into_iter()
        .map(|mut command| {
            command.description = description(&command.name).into();
            command
        })
        .collect()
}

/// Bounded command overlay immediately above the input.
pub fn popup(text: &str, selected: usize, width: usize, capacity: usize) -> Vec<String> {
    let choices = candidates(text);
    let rows = choices
        .iter()
        .map(|command| {
            format!(
                "/{}  {}  [{}]",
                command.name,
                command.description,
                category(&command.name)
            )
        })
        .collect::<Vec<_>>();
    let body = super::widgets::list(&rows, selected, capacity, width.saturating_sub(2));
    super::widgets::Frame {
        title: "Команды",
        footer: "↑↓ выбор · tab вставить · enter выполнить · esc закрыть",
        glyphs: super::widgets::Glyphs::detect(),
    }
    .render(&body, width)
}

fn home_dir(fallback: &Path) -> PathBuf {
    crate::platform::home_dir().unwrap_or_else(|| fallback.to_path_buf())
}

fn mode_label(mode: Mode) -> &'static str {
    match mode {
        Mode::Default => "DEFAULT",
        Mode::AcceptEdits => "ACCEPT EDITS",
        Mode::Plan => "PLAN",
        Mode::Bypass => "BYPASS",
    }
}

/// Cycle the active permission mode (`Default` → `AcceptEdits` → `Plan` → `Bypass` → `Default`)
/// and return a structured status card.
pub fn cycle_permission_card(agent: &mut Agent, status: &mut Status, width: usize) -> Vec<String> {
    let next = match agent.tools.permissions.mode {
        Mode::Default => Mode::AcceptEdits,
        Mode::AcceptEdits => Mode::Plan,
        Mode::Plan => Mode::Bypass,
        Mode::Bypass => Mode::Default,
    };
    agent.tools.permissions.mode = next;
    let label = mode_label(next);
    status.permission = label.into();
    status.toast = Some(format!("Режим доступа: [{label}]"));
    let desc = match next {
        Mode::Default => "Запрашивать подтверждение перед записью и командами",
        Mode::AcceptEdits => "Автоматически разрешать правки файлов в рабочей папке",
        Mode::Plan => "Только чтение и планирование (мутации заблокированы)",
        Mode::Bypass => "Выполнять инструменты без запросов (явные deny сохраняются)",
    };
    let body = vec![
        format!("Активный режим: [{}]", label),
        format!("Описание: {desc}"),
    ];
    widgets::card("Режим доступа", &body, "shift+tab переключить", width)
}

/// Render a transcript summary card of the conversation (`Ctrl+O`).
pub fn transcript_card(messages: &[Message], width: usize) -> Vec<String> {
    let mut body = Vec::new();
    if messages.is_empty() {
        body.push("Журнал разговора пуст.".into());
        body.push("Отправь запрос модели, чтобы увидеть полную расшифровку.".into());
    } else {
        body.push(format!("Всего сообщений в контексте: {}", messages.len()));
        for (idx, msg) in messages.iter().enumerate().take(12) {
            let role = match msg.role {
                Role::User => "Пользователь",
                Role::Assistant => "Ассистент",
                Role::System => "Система",
            };
            let snippet = msg
                .content
                .first()
                .map(|c| match c {
                    Content::Text { text } => text.lines().next().unwrap_or("").to_owned(),
                    Content::ToolUse { name, .. } => format!("[инструмент: {name}]"),
                    Content::ToolResult { is_error, .. } => {
                        if *is_error {
                            "[результат: ошибка]".into()
                        } else {
                            "[результат инструмента]".into()
                        }
                    }
                    Content::Thinking { .. } => "[размышление]".into(),
                    _ => "[вложение]".into(),
                })
                .unwrap_or_default();
            body.push(format!(" #{:<2} {:<12} {}", idx + 1, role, snippet));
        }
    }
    widgets::card("Расшифровка", &body, "ctrl+o закрыть", width)
}

/// Render a checklist card (`Ctrl+T`) inspecting any recorded `TodoWrite` calls in the conversation.
pub fn todos_card(messages: &[Message], width: usize) -> Vec<String> {
    let mut list = crate::agent::agency::TodoList::new();
    for msg in messages {
        for part in &msg.content {
            if let Content::ToolUse { name, input, .. } = part {
                if name == "TodoWrite" {
                    let _ = list.update_from_value(input);
                }
            }
        }
    }
    let mut body = Vec::new();
    if list.items.is_empty() {
        body.push("Список задач пуст.".into());
        body.push("Агент добавит задачи автоматически при многошаговой работе.".into());
    } else {
        for item in &list.items {
            let mark = match item.status {
                crate::agent::agency::TodoStatus::Pending => "[ ]",
                crate::agent::agency::TodoStatus::InProgress => "[*]",
                crate::agent::agency::TodoStatus::Completed => "[✓]",
            };
            body.push(format!(" {mark} {} (#{})", item.content, item.id));
        }
    }
    widgets::card("Задачи сессии", &body, "ctrl+t скрыть", width)
}

/// Render a history search card (`Ctrl+R`) for the editor's session history.
pub fn history_card(history: &[String], current: &str, width: usize) -> Vec<String> {
    let query = current.trim().to_lowercase();
    let mut body = Vec::new();
    let matches: Vec<&String> = history
        .iter()
        .rev()
        .filter(|entry| query.is_empty() || entry.to_lowercase().contains(&query))
        .take(8)
        .collect();
    if matches.is_empty() {
        body.push("Совпадений в истории ввода не найдено.".into());
        body.push("Используй ↑ / ↓ для навигации по предыдущим запросам.".into());
    } else {
        body.push(format!("Найдено записей: {}", matches.len()));
        for (idx, item) in matches.iter().enumerate() {
            let line = item.lines().next().unwrap_or("");
            body.push(format!(" {:>2}. {line}", idx + 1));
        }
    }
    widgets::card(
        "История запросов",
        &body,
        "↑↓ навигация · esc закрыть",
        width,
    )
}

/// Execute any built-in slash command and return its rendered `Card` rows (or `None` when `prompt`
/// is not a slash command). Every one of the 34 built-in commands (`BUILTIN_COMMANDS` + `/new`)
/// produces a structured card with real workspace/session data.
pub async fn execute_command(
    prompt: &str,
    agent: &mut Agent,
    messages: &mut Vec<Message>,
    status: &mut Status,
    width: usize,
) -> io::Result<Option<Vec<String>>> {
    let Some(command) = prompt.trim().strip_prefix('/') else {
        return Ok(None);
    };
    let (name, args) = command
        .split_once(char::is_whitespace)
        .unwrap_or((command, ""));
    let args = args.trim();
    let width = width.max(20);
    let inner = width.saturating_sub(2).max(10);
    let glyphs = Glyphs::detect();
    let root = agent.tools.permissions.root.clone();
    let home = home_dir(&root);

    if name == "usage" {
        if !args.is_empty() {
            return Ok(Some(widgets::card(
                "/usage",
                &["Команда /usage не поддерживает аргументы.".into()],
                "/usage статистика",
                width,
            )));
        }
        let home_clone = home.clone();
        let entries = tokio::task::spawn_blocking(move || {
            UsageLedger::open(&home_clone).and_then(|l| l.entries(None))
        })
        .await
        .map_err(io::Error::other)?;
        let entries = match entries {
            Ok(entries) => entries,
            Err(_) => return Ok(Some(widgets::card("/usage", &["Не удалось прочитать журнал использования; это не означает отсутствие запросов.".into()], "/doctor проверить доступ", width))),
        };
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        return Ok(Some(usage_panel::render(
            &entries, "", now, 0, false, width,
        )));
    }

    let mut body = Vec::new();
    if !args.is_empty()
        && !matches!(
            name,
            "config"
                | "model"
                | "resume"
                | "rewind"
                | "plan"
                | "permissions"
                | "commit"
                | "export"
                | "add-dir"
        )
    {
        return Ok(Some(widgets::card_with_glyphs(
            &format!("/{name}"),
            &[format!(
                "Команда /{name} не поддерживает аргументы. Ничего не изменено."
            )],
            "/help справка по командам",
            width,
            glyphs,
        )));
    }
    let footer: &str = match name {
        "help" => {
            for group in [
                "сессия",
                "модель",
                "контекст",
                "инструменты",
                "git",
                "система",
            ] {
                body.push(format!("[{group}]"));
                body.extend(
                    BUILTIN_COMMANDS
                        .iter()
                        .filter(|c| category(c.name) == group)
                        .map(|c| format!(" /{:<15} {}", c.name, description(c.name))),
                );
                body.push(String::new());
            }
            body.extend([
                "Клавиши: Shift+Enter — новая строка · Shift+Tab — режим доступа".into(),
                "Ctrl+C — очистить/прервать · Ctrl+D — выход · / — команды".into(),
            ]);
            "↑↓ прокрутка · /команда выполнить"
        }
        "clear" | "new" => {
            let removed = messages.len();
            messages.clear();
            status.toast = Some("Контекст очищен".into());
            body.extend([
                "Контекст активного разговора очищен.".into(),
                format!("Удалено сообщений из памяти сессии: {removed}"),
                "Сохранённые на диске журналы сессий не затронуты (/resume).".into(),
            ]);
            "/resume история сессий"
        }
        "compact" => {
            let before = context::estimate(messages).map_err(io::Error::other)?;
            let count = context::trim_tool_results(messages, 4, 1024);
            let after = context::estimate(messages).map_err(io::Error::other)?;
            let saved = before.saturating_sub(after);
            let ratio = if before > 0 {
                (after as f64 / before as f64).clamp(0.0, 1.0)
            } else {
                0.0
            };
            body.extend([
                format!("Токены контекста: ~{before} → ~{after} (экономия ~{saved})"),
                format!(
                    "Заполнение: [{}] {:.0}%",
                    widgets::progress(ratio, inner.saturating_sub(22).clamp(6, 24), glyphs),
                    ratio * 100.0
                ),
                format!("Сокращено результатов инструментов: {count}"),
            ]);
            "/context подробный состав"
        }
        "config" => {
            if let Some((k, v)) = args.split_once(char::is_whitespace) {
                let k = k.trim();
                let v = v.trim();
                match k {
                    "model" if !v.is_empty() => {
                        agent.model = v.into();
                        status.model = v.into();
                    }
                    "max_turns" => {
                        match v.parse::<u32>() {
                            Ok(turns) if turns > 0 => agent.max_turns = turns,
                            _ => body.push("max_turns должен быть целым положительным числом; значение не изменено.".into()),
                        }
                    }
                    _ => body.push("Этот параметр нельзя изменить здесь. Поддерживаются model и max_turns.".into()),
                }
            } else if !args.is_empty() {
                body.push(
                    "Для изменения укажи /config model <имя> или /config max_turns <число>.".into(),
                );
            }
            body.push(
                "Изменения действуют только в текущем процессе; файл конфигурации не сохраняется."
                    .into(),
            );
            let rows = vec![
                vec!["model".into(), agent.model.clone()],
                vec!["permission_mode".into(), status.permission.clone()],
                vec!["max_turns".into(), agent.max_turns.to_string()],
                vec!["max_tokens".into(), agent.max_tokens.to_string()],
                vec!["language".into(), "ru".into()],
                vec!["workspace".into(), root.display().to_string()],
            ];
            body.push("Активные параметры окружения и сессии:".into());
            body.extend(
                Table {
                    headers: &["Параметр", "Значение"],
                    rows: &rows,
                    numeric: &[],
                }
                .render(inner),
            );
            "/config <ключ> <значение>"
        }
        "model" => {
            if !args.is_empty() {
                agent.model = args.into();
                status.model = agent.model.clone();
                status.toast = Some(format!("Модель: {}", agent.model));
            }
            let presets = [
                ("claude-sonnet-4-20250514", "Anthropic · код и агенты"),
                ("gpt-4.1", "OpenAI · универсальная"),
                ("deepseek-chat", "OpenAI-совместимая · быстрая"),
            ];
            body.push(format!(
                "Активная модель: [{}] {}",
                widgets::chip("активна"),
                agent.model
            ));
            body.push("Примеры идентификаторов (доступность у провайдера не проверена):".into());
            for (id, note) in presets {
                let mark = if id == agent.model { ">" } else { " " };
                body.push(format!(" {mark} {id:<26} {note}"));
            }
            "/model <имя-модели> сменить"
        }
        "provider" => {
            body.extend([
                format!("Активная модель: {}", agent.model),
                "Протоколы транспорта: Anthropic Messages · OpenAI Chat · OpenAI Responses".into(),
                "Авторизация: заголовок x-api-key или Authorization Bearer".into(),
                "Переопределение: флаг --base-url или --protocol в командной строке".into(),
            ]);
            "/login настройка ключа · /doctor проверка"
        }
        "cost" => {
            let cost_str = status
                .cost_usd
                .filter(|n| n.is_finite() && *n >= 0.0)
                .map_or_else(|| "цена модели не задана".into(), |n| format!("${n:.4}"));
            let rows = vec![
                vec!["Модель".into(), status.model.clone()],
                vec!["Токены входа".into(), compact(status.input)],
                vec!["Токены выхода".into(), compact(status.output)],
                vec![
                    "Кэш (чтение / запись)".into(),
                    format!(
                        "{} / {}",
                        compact(status.cache_read),
                        compact(status.cache_write)
                    ),
                ],
                vec!["Оценка стоимости".into(), cost_str],
            ];
            body.extend(
                Table {
                    headers: &["Показатель", "Значение"],
                    rows: &rows,
                    numeric: &[1],
                }
                .render(inner),
            );
            "/usage подробная статистика по периодам"
        }
        "status" => {
            let branch = if status.branch.is_empty() {
                "нет данных"
            } else {
                &status.branch
            };
            let rows = vec![
                vec!["Модель".into(), agent.model.clone()],
                vec!["Режим доступа".into(), status.permission.clone()],
                vec!["Рабочая папка".into(), root.display().to_string()],
                vec![
                    "Ветка git".into(),
                    format!("{branch}{}", if status.dirty { " *" } else { "" }),
                ],
                vec!["Сообщений в сессии".into(), messages.len().to_string()],
                vec![
                    "Токены (вход / выход)".into(),
                    format!("{} / {}", compact(status.input), compact(status.output)),
                ],
            ];
            body.extend(
                Table {
                    headers: &["Поле", "Состояние"],
                    rows: &rows,
                    numeric: &[],
                }
                .render(inner),
            );
            "/doctor полная диагностика"
        }
        "context" => {
            let sys_tokens = agent.system.len().div_ceil(4) as u64;
            let tools_tokens = (crate::tools::Tools::definitions().len() * 140) as u64;
            let mem_tokens = std::fs::read_to_string(root.join("GITCODE.md"))
                .map(|s| s.len().div_ceil(4) as u64)
                .unwrap_or(0);
            let msg_tokens = context::estimate(messages).map_err(io::Error::other)?;
            let used = sys_tokens + tools_tokens + mem_tokens + msg_tokens;
            body.push("Размер окна условный: 200k; оценки по тексту, не данные провайдера.".into());
            let window = 200_000u64;
            let free = window.saturating_sub(used);
            let ratio = (used as f64 / window as f64).clamp(0.0, 1.0);
            body.push(format!(
                "Занято контекста: ~{} из {} токенов ({:.1}%)",
                compact(used),
                compact(window),
                ratio * 100.0
            ));
            body.push(widgets::progress(ratio, inner.min(48), glyphs));
            let segments = [
                ("системный промпт", sys_tokens),
                ("инструменты", tools_tokens),
                ("файлы памяти", mem_tokens),
                ("сообщения", msg_tokens),
                ("свободно", free),
            ];
            let rows = segments
                .into_iter()
                .map(|(label, tok)| {
                    let pct = (tok as f64 / window as f64) * 100.0;
                    vec![
                        label.into(),
                        format!("~{}", compact(tok)),
                        format!("{pct:.1}%"),
                    ]
                })
                .collect::<Vec<_>>();
            body.extend(
                Table {
                    headers: &["Сегмент", "Токены", "Доля"],
                    rows: &rows,
                    numeric: &[1, 2],
                }
                .render(inner),
            );
            "/compact сжать результаты инструментов"
        }
        "resume" => {
            let home_clone = home.clone();
            let root_clone = root.clone();
            if !args.is_empty() {
                let id = args.to_owned();
                let restored = tokio::task::spawn_blocking(move || {
                    SessionStore::open(&home_clone, &root_clone)?.resume(&id)
                })
                .await
                .map_err(io::Error::other)?;
                match restored {
                    Ok(session) => {
                        let count = session.messages.len();
                        *messages = session.messages;
                        status.toast = Some(format!("Загружено сообщений: {count}"));
                        body.push(format!("Контекст загружен из сессии {}.", session.id));
                        body.push(format!(
                            "Заголовок: {} · сообщений: {count}",
                            session.metadata.title
                        ));
                        body.push("Восстановлен только контекст: активный журнал сохранения, счётчики и файлы проекта не переключены.".into());
                        if session.interrupted_bytes > 0 {
                            body.push(
                                "Незавершённый хвост журнала пропущен при восстановлении.".into(),
                            );
                        }
                    }
                    Err(_) => {
                        body.push("Не удалось загрузить сессию: проверь ID и доступность журнала этого проекта.".into());
                        body.push("Текущий контекст не изменён.".into());
                    }
                }
                return Ok(Some(widgets::card_with_glyphs(
                    "/resume",
                    &body,
                    "/resume список сессий",
                    width,
                    glyphs,
                )));
            }
            let sessions = tokio::task::spawn_blocking(move || {
                SessionStore::open(&home_clone, &root_clone).and_then(|s| s.list())
            })
            .await
            .map_err(io::Error::other)?;
            match sessions {
                Ok(list) if !list.is_empty() => {
                    let rows = list
                        .iter()
                        .take(8)
                        .map(|s| {
                            vec![
                                s.id.clone(),
                                s.metadata.title.clone(),
                                s.message_count.to_string(),
                                s.metadata.branch.clone().unwrap_or_else(|| "—".into()),
                            ]
                        })
                        .collect::<Vec<_>>();
                    body.extend(
                        Table {
                            headers: &["ID", "Заголовок", "Сообщ.", "Ветка"],
                            rows: &rows,
                            numeric: &[2],
                        }
                        .render(inner),
                    );
                }
                Ok(_) => {
                    body.push("Сохранённых сессий для этого проекта пока нет.".into());
                    body.push("Сессии автоматически сохраняются после завершения хода.".into());
                }
                Err(_) => body.push(
                    "Не удалось прочитать журналы сессий. Проверь доступ и целостность файлов."
                        .into(),
                ),
            }
            "/resume <id> загрузить контекст"
        }
        "rewind" => {
            body.push("Меняется только список сообщений; файлы проекта и журнал на диске не откатываются.".into());
            if let Ok(idx) = args.parse::<usize>() {
                if idx < messages.len() {
                    messages.truncate(idx);
                    body.push(format!("Разговор перемотан к сообщению #{idx}."));
                } else {
                    body.push(format!(
                        "Индекс {idx} вне диапазона (всего сообщений: {}).",
                        messages.len()
                    ));
                }
            } else {
                body.push(format!(
                    "Доступно точек возврата в текущей сессии: {}",
                    messages.len()
                ));
                for (i, msg) in messages.iter().enumerate().rev().take(5) {
                    let role = match msg.role {
                        Role::User => "пользователь",
                        Role::Assistant => "ассистент",
                        Role::System => "система",
                    };
                    body.push(format!(" [#{i}] ход ({role})"));
                }
                if messages.is_empty() {
                    body.push(
                        "История сообщений пуста — отправь запрос для создания контекста.".into(),
                    );
                }
            }
            "/rewind <индекс-сообщения> удалить контекст начиная с индекса"
        }
        "plan" => {
            agent.tools.permissions.mode = Mode::Plan;
            status.permission = "PLAN".into();
            body.extend([
                "Режим планирования [PLAN] активирован.".into(),
                "Разрешены только инструменты чтения и исследования (Read, LS, Glob, Grep).".into(),
                "Инструменты изменения файлов и запуска команд заблокированы до одобрения плана."
                    .into(),
            ]);
            if !args.is_empty() {
                body.push(format!("Задача для планирования: {args}"));
            }
            "shift+tab сменить режим"
        }
        "permissions" => {
            if !args.is_empty() {
                let (kind, text) = args.split_once(char::is_whitespace).unwrap_or((args, ""));
                if matches!(kind, "allow" | "ask" | "deny") {
                    match Rule::parse(text.trim()) {
                        Ok(rule) => {
                            match kind {
                                "allow" => agent.tools.permissions.allow.push(rule),
                                "ask" => agent.tools.permissions.ask.push(rule),
                                _ => agent.tools.permissions.deny.push(rule),
                            }
                            body.push("Правило добавлено только в текущий процесс; на диск не сохраняется.".into());
                        }
                        Err(_) => {
                            body.push("Правило не добавлено: нужен формат Tool(шаблон).".into())
                        }
                    }
                } else {
                    body.push("Поддерживается /permissions allow|ask|deny Tool(шаблон); удаление правил не реализовано.".into());
                }
            }
            body.push(format!("Активный режим: [{}]", status.permission));
            let rows = vec![
                vec![
                    "Запретить (deny)".into(),
                    agent.tools.permissions.deny.len().to_string(),
                ],
                vec![
                    "Спросить (ask)".into(),
                    agent.tools.permissions.ask.len().to_string(),
                ],
                vec![
                    "Разрешить (allow)".into(),
                    agent.tools.permissions.allow.len().to_string(),
                ],
            ];
            body.extend(
                Table {
                    headers: &["Категория правил", "Количество"],
                    rows: &rows,
                    numeric: &[1],
                }
                .render(inner),
            );
            for (label, rules) in [
                ("deny", &agent.tools.permissions.deny),
                ("ask", &agent.tools.permissions.ask),
                ("allow", &agent.tools.permissions.allow),
            ] {
                for r in rules.iter().take(4) {
                    body.push(format!(" [{label}] {}({})", r.tool, r.pattern));
                }
            }
            "shift+tab переключить режим · /permissions allow <правило>"
        }
        "agents" => {
            let home_clone = home.clone();
            let root_clone = root.clone();
            let profiles =
                tokio::task::spawn_blocking(move || load_agents(&home_clone, &root_clone))
                    .await
                    .map_err(io::Error::other)?
                    .unwrap_or_else(|_| crate::agent::agency::SubagentProfile::builtins());
            let rows = profiles
                .iter()
                .map(|p| {
                    vec![
                        p.name.clone(),
                        if p.read_only {
                            "только чтение".into()
                        } else {
                            "чтение и запись".into()
                        },
                        p.description.clone(),
                    ]
                })
                .collect::<Vec<_>>();
            body.extend(
                Table {
                    headers: &["Агент", "Доступ", "Описание"],
                    rows: &rows,
                    numeric: &[],
                }
                .render(inner),
            );
            ".gitcode/agents/*.md свои агенты"
        }
        "mcp" => {
            let mcp_path = root.join(".mcp.json");
            match list_mcp_servers(&mcp_path) {
                Ok(servers) if !servers.is_empty() => {
                    let rows = servers
                        .into_iter()
                        .map(|(srv_name, cfg)| {
                            let (transport, target) = match cfg {
                                McpServerConfig::Stdio { command, .. } => ("stdio", command),
                                McpServerConfig::Http { url, .. } => ("http", url),
                            };
                            vec![srv_name, transport.into(), target]
                        })
                        .collect::<Vec<_>>();
                    body.extend(
                        Table {
                            headers: &["Сервер", "Транспорт", "Команда / URL"],
                            rows: &rows,
                            numeric: &[],
                        }
                        .render(inner),
                    );
                }
                _ => {
                    body.push("Серверы MCP в файле .mcp.json не настроены.".into());
                    body.push(
                        "Добавь конфигурацию сервера в .mcp.json или через gitcode mcp.".into(),
                    );
                }
            }
            ".mcp.json конфигурация серверов"
        }
        "init" => {
            let guide_path = root.join("GITCODE.md");
            if guide_path.exists() {
                body.push(format!(
                    "Файл инструкций уже существует: {}",
                    guide_path.display()
                ));
                body.push("Используй /memory для просмотра содержимого.".into());
            } else {
                let template = "# Инструкции проекта для gitcode\n\n- Команда сборки и проверки: `cargo test`\n- Стиль кода: идиоматичный, без лишних зависимостей.\n";
                match agent
                    .tools
                    .execute(
                        "Write",
                        &serde_json::json!({
                            "path": "GITCODE.md", "content": template
                        }),
                    )
                    .await
                {
                    Ok(_) => {
                        body.push(format!(
                            "Создан шаблон инструкций: {}",
                            guide_path.display()
                        ));
                        body.push(
                            "Отредактируй GITCODE.md, чтобы закрепить правила репозитория.".into(),
                        );
                    }
                    Err(_) => {
                        body.push("GITCODE.md не создан: запись запрещена, требует подтверждения или путь недоступен.".into());
                        body.push("Команда не обходит правила доступа. Проверь /permissions; в режиме PLAN запись запрещена.".into());
                    }
                }
            }
            "/memory просмотр GITCODE.md"
        }
        "review" => {
            let output = tokio::process::Command::new("git")
                .args(["status", "--short", "--branch"])
                .current_dir(&root)
                .output()
                .await;
            match output {
                Ok(out) if out.status.success() => {
                    let text = String::from_utf8_lossy(&out.stdout);
                    body.push("Обзор рабочей копии (git status):".into());
                    for line in text.lines().take(12) {
                        body.push(format!(" {line}"));
                    }
                    if text.lines().count() <= 1 {
                        body.push(" Незакоммиченных изменений не обнаружено.".into());
                    }
                }
                _ => {
                    body.push(
                        "Рабочая папка не является git-репозиторием или git недоступен.".into(),
                    );
                }
            }
            "/diff подробные изменения по строкам"
        }
        "diff" => {
            let output = tokio::process::Command::new("git")
                .args(["diff", "--no-ext-diff", "--no-color"])
                .current_dir(&root)
                .output()
                .await;
            match output {
                Ok(out) if out.status.success() => {
                    let diff = String::from_utf8_lossy(&out.stdout);
                    if diff.trim().is_empty() {
                        body.push("Незакоммиченных изменений в отслеживаемых файлах нет.".into());
                    } else {
                        for line in diff.lines().take(18) {
                            body.push(line.to_owned());
                        }
                    }
                }
                _ => {
                    body.push(
                        "Не удалось прочитать diff; проверь, что папка — git-репозиторий.".into(),
                    );
                }
            }
            "/review сводка по файлам · /commit коммит"
        }
        "commit" => {
            body.push(
                "Коммит не создаётся: показана только готовность и черновик сообщения.".into(),
            );
            let output = tokio::process::Command::new("git")
                .args(["status", "--porcelain"])
                .current_dir(&root)
                .output()
                .await;
            match output {
                Ok(out) if out.status.success() => {
                    let changes: Vec<String> = String::from_utf8_lossy(&out.stdout)
                        .lines()
                        .map(str::to_owned)
                        .collect();
                    if changes.is_empty() {
                        body.push("Нет изменённых файлов для создания коммита.".into());
                    } else {
                        body.push(format!("Изменённых путей к коммиту: {}", changes.len()));
                        for line in changes.iter().take(8) {
                            body.push(format!(" {line}"));
                        }
                        if !args.is_empty() {
                            body.push(format!("Черновик сообщения: {args}"));
                        } else {
                            body.push("Передай сообщение `/commit <текст>` или попроси агента создать коммит.".into());
                        }
                    }
                }
                _ => {
                    body.push("Не удалось проверить статус git в рабочей папке.".into());
                }
            }
            "/diff проверить изменения перед коммитом"
        }
        "memory" => {
            let path = root.join("GITCODE.md");
            match tokio::fs::read_to_string(&path).await {
                Ok(text) => {
                    body.push(format!("Файл памяти: {}", path.display()));
                    for line in text.lines().take(14) {
                        body.push(format!(" {line}"));
                    }
                }
                Err(_) => {
                    body.push("Файл GITCODE.md отсутствует в корне проекта.".into());
                    body.push("Выполни /init для создания стартового шаблона инструкций.".into());
                }
            }
            "/init создать GITCODE.md"
        }
        "doctor" => {
            let has_key = std::env::var_os("ANTHROPIC_API_KEY").is_some()
                || std::env::var_os("OPENAI_API_KEY").is_some()
                || std::env::var_os("GITCODE_API_KEY").is_some()
                || home.join(".gitcode").join("credentials.toml").is_file();
            let report = run_doctor(&root, true, has_key);
            let status_word = if report.healthy {
                "исправно"
            } else {
                "требует внимания"
            };
            body.push(format!("Общее состояние окружения: [{status_word}]"));
            for check in &report.checks {
                let mark = if check.ok { "✓" } else { "✗" };
                let check_ru = match check.name.as_str() {
                    "git" => "Система контроля версий git",
                    "workspace" => "Рабочая директория проекта",
                    "config" => "Конфигурация приложения",
                    "credentials" => "Ключ авторизации провайдера",
                    other => other,
                };
                body.push(format!(" {mark} {check_ru}: {}", check.detail));
            }
            "/login настроить ключ доступа"
        }
        "theme" => {
            body.extend([
                "Активная тема: Тёмная сакура (плоская розово-фиолетовая палитра, без градиентов)"
                    .into(),
                "Акцент и фокус: розовый (#ff6fa9) · Структура и рамки: фиолетовый (#6a4c93)"
                    .into(),
                "Режимы совместимости: NO_COLOR=1 (монохромный) · GITCODE_ASCII=1 (ASCII рамки)"
                    .into(),
            ]);
            "NO_COLOR=1 отключить цвет"
        }
        "vim" => {
            body.push(
                "Режим Vim не реализован и не включён; ниже — доступные сочетания клавиш.".into(),
            );
            body.extend([
                "Режим редактирования ввода: гибридный (Readline + многострочный редактор).".into(),
                "Навигация: Ctrl+A / Ctrl+E (начало/конец строки), Ctrl+W (удалить слово).".into(),
                "Многострочный ввод: Shift+Enter или обратная косая черта `\\` + Enter.".into(),
            ]);
            "/terminal-setup справка по клавишам"
        }
        "export" => {
            if args.is_empty() {
                body.push(
                    "Укажи относительный путь внутри проекта: /export conversation.json".into(),
                );
                body.push(format!("Сообщений, готовых к экспорту: {}", messages.len()));
            } else {
                let data = serde_json::to_string_pretty(messages).map_err(io::Error::other)?;
                match agent
                    .tools
                    .execute("Write", &serde_json::json!({"path": args, "content": data}))
                    .await
                {
                    Ok(_) => body.push(format!("Разговор успешно сохранён в файл: {args}")),
                    Err(_) => body.push(
                        "Сохранение отклонено правилами доступа или путь вне рабочей папки. Проверь /permissions."
                            .into(),
                    ),
                }
            }
            "/export <путь.json>"
        }
        "copy" => {
            let last_assistant = messages.iter().rev().find(|m| m.role == Role::Assistant);
            if let Some(msg) = last_assistant {
                let text = msg
                    .content
                    .iter()
                    .filter_map(|c| match c {
                        Content::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                let chars = text.chars().count();
                body.push("Копирование не выполнено: системный буфер обмена не подключён.".into());
                body.push(format!("Последний ответ содержит {chars} симв. Используй /export <путь.json> для сохранения."));
                if let Some(first) = text.lines().next() {
                    body.push(format!("Фрагмент: {first}"));
                }
            } else {
                body.push("В текущей сессии ещё нет ответов ассистента для копирования.".into());
            }
            "/export сохранить весь диалог в файл"
        }
        "hooks" => {
            body.extend([
                "Поддерживаемые события жизненного цикла (hooks):".into(),
                " • PreToolUse / PostToolUse — проверка и аудит вызовов инструментов".into(),
                " • UserPromptSubmit / Stop / SubagentStop — обработка хода".into(),
                " • SessionStart / SessionEnd / Notification / PreCompact".into(),
                "Код возврата 2 блокирует действие и передаёт причину модели.".into(),
            ]);
            ".gitcode/settings.json настройка хуков"
        }
        "skills" => {
            let home_clone = home.clone();
            let root_clone = root.clone();
            let skills =
                tokio::task::spawn_blocking(move || discover_skills(&home_clone, &root_clone))
                    .await
                    .map_err(io::Error::other)??;
            if skills.is_empty() {
                body.push("Навыков пока не обнаружено.".into());
                body.push(
                    "Добавь файл .gitcode/skills/<имя>/SKILL.md или ~/.gitcode/skills/<имя>/SKILL.md."
                        .into(),
                );
            } else {
                for skill in skills {
                    body.push(format!(" • {} — {}", skill.name, skill.description));
                }
            }
            ".gitcode/skills/<имя>/SKILL.md"
        }
        "add-dir" => {
            body.push("Дополнительные корни не добавляются; команда только проверяет папку внутри проекта.".into());
            body.push(format!("Основная рабочая папка: {}", root.display()));
            if args.is_empty() {
                body.push("Укажи путь для проверки доступа: /add-dir <папка>".into());
            } else {
                let candidate = root.join(args);
                match agent.tools.permissions.existing(&candidate) {
                    Ok(resolved) if resolved.is_dir() => {
                        body.push(format!(
                            "Директория подтверждена внутри границ проекта: {}",
                            resolved.display()
                        ));
                    }
                    _ => {
                        body.push(
                            "Путь не является доступной папкой внутри рабочей области.".into(),
                        );
                    }
                }
            }
            "/permissions правила доступа к путям"
        }
        "login" => {
            let cred_path = home.join(".gitcode").join("credentials.toml");
            body.extend([
                format!("Файл учётных данных: {}", cred_path.display()),
                "Права доступа к файлу: 0600 (только владелец).".into(),
                "Переменные окружения: ANTHROPIC_API_KEY, OPENAI_API_KEY, GITCODE_API_KEY.".into(),
                "Ключ передаётся исключительно через HTTP-заголовок и не попадает в логи.".into(),
            ]);
            "/doctor проверить наличие ключа"
        }
        "logout" => {
            let cred_path = home.join(".gitcode").join("credentials.toml");
            if cred_path.exists() {
                body.push(format!(
                    "Обнаружен файл учётных данных: {}",
                    cred_path.display()
                ));
                body.push(
                    "Удали секцию провайдера из credentials.toml или очисти переменную окружения."
                        .into(),
                );
            } else {
                body.push("Сохранённый файл credentials.toml отсутствует на диске.".into());
                body.push("Активных сохранённых ключей на диске нет.".into());
            }
            "/login справка по авторизации"
        }
        "terminal-setup" => {
            let rows = vec![
                vec![
                    "Shift+Enter или \\ + Enter".into(),
                    "Новая строка в поле ввода".into(),
                ],
                vec![
                    "Shift+Tab".into(),
                    "Переключить режим доступа (DEFAULT/PLAN/...)".into(),
                ],
                vec![
                    "Ctrl+C".into(),
                    "Очистить ввод или прервать текущий ход".into(),
                ],
                vec![
                    "Ctrl+D".into(),
                    "Закрыть приложение (на пустой строке)".into(),
                ],
                vec![
                    "Ctrl+O / Ctrl+T / Ctrl+R".into(),
                    "Расшифровка / Задачи / История".into(),
                ],
                vec![
                    "Esc Esc".into(),
                    "Открыть контрольные точки (/rewind)".into(),
                ],
            ];
            body.extend(
                Table {
                    headers: &["Сочетание клавиш", "Действие"],
                    rows: &rows,
                    numeric: &[],
                }
                .render(inner),
            );
            "/help все команды"
        }
        "exit" => {
            body.push("Завершение сессии gitcode. До встречи!".into());
            "ctrl+d выход"
        }
        _ => {
            body.push(description(name).into());
            body.push("Пользовательская команда или расширение.".into());
            "/help список встроенных команд"
        }
    };

    Ok(Some(widgets::card_with_glyphs(
        &format!("/{name}"),
        &body,
        footer,
        width,
        glyphs,
    )))
}

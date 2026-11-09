# M19 — итоговый фактический отчёт (2026-11-09)

Репозиторий: https://github.com/jaddd14/gitcode-agent

## Подтверждено

- `feat/windows` включена в remote `main` fast-forward до `c80463b`, без force-push и изменения рабочего дерева main.
- CI ветки: https://github.com/jaddd14/gitcode-agent/actions/runs/42582720748 — Linux/macOS/Windows и три installer smoke успешны. Security: https://github.com/jaddd14/gitcode-agent/actions/runs/42582720700 — успешно.
- CI main: https://github.com/jaddd14/gitcode-agent/actions/runs/42583528523 — успешно. Security main: https://github.com/jaddd14/gitcode-agent/actions/runs/42583528530 — успешно.
- Релиз https://github.com/jaddd14/gitcode-agent/releases/tag/v0.3.0 — Latest, не draft/prerelease; пять platform archives, SHA256SUMS, оба installer и latest.json. Pipeline https://github.com/jaddd14/gitcode-agent/actions/runs/42583690059 — успешно.
- `dist/gitcode/` собран из опубликованных assets; SHA256 всех пяти архивов совпадают. Staging deployment `7650038678` и release deployment `7650207520` — success.
- Stable copy, пользовательская конфигурация, proxy/server config и services не менялись.

## Установка

Windows PowerShell:

```powershell
$env:GITCODE_BASE_URL = 'https://github.com/jaddd14/gitcode-agent/releases/latest/download'
irm https://raw.githubusercontent.com/jaddd14/gitcode-agent/main/scripts/install.ps1 | iex
```

Linux/macOS:

```sh
export GITCODE_BASE_URL=https://github.com/jaddd14/gitcode-agent/releases/latest/download
curl -fsSL https://raw.githubusercontent.com/jaddd14/gitcode-agent/main/scripts/install.sh | bash
```

Обновление: `gitcode update`. На совместимых runner-системах native installer smokes успешны. На текущем Ubuntu 22.04 arm64 installer скачал и проверил v0.3.0, но запуск опубликованного бинарника завершился ошибкой: требуется GLIBC_2.39, здесь glibc 2.35. Поэтому local published-binary update smoke не выполнен; на старом Linux нужна сборка из исходников. Не выдаём установку на этом worker за успешный запуск.

## Домен и ограничения

`https://api.samp.team/gitcode/latest.json` возвращает HTTP 404: домен **не включён**, его one-liners не рекомендуются. После проверки собственного сервера загрузить файлы:

```sh
rsync -av dist/gitcode/ <user>@<host>:/srv/www/api/gitcode/
```

Сниппеты nginx/Caddy и проверки endpoint находятся в [DEPLOY-INSTALLER.md](DEPLOY-INSTALLER.md). Существующие конфигурации и services не менялись.

Windows sandbox — tool-level, не OS isolation; бинарники не подписаны (SmartScreen возможен). Для newline доступны Alt+Enter, Ctrl+J, trailing backslash. GUI clipboard/toast display не проверены на этом Linux worker. Toast использует существующий PowerShell application ID, подчиняется политике уведомлений ОС и не гарантирует показ. Intro не менялся.

## Незакрытые пункты полного task2

Релиз `v0.2.0` отсутствует в GitHub (опубликованы v0.1.0 и v0.3.0); исторический GLOWUP-REPORT отсутствовал. Не создаём фиктивный исторический релиз и не утверждаем M18 publication acceptance. Это не препятствует факту публикации Windows v0.3.0, но полная task2 acceptance и DONE пока не подтверждены. Требуется отдельно решить пропущенную публикацию M18 без изменения защищённой stable copy. Рекомендуется также проверить GUI на Windows Terminal и улучшить базовую glibc-совместимость Linux сборок.

GitHub-токен необходимо отозвать/заменить: при первом неудачном push исключение Python вывело авторизованную команду в transcript. Токен не сохранён в репозитории, remote или gitconfig; значение здесь не повторяется.

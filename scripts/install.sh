#!/bin/sh
# Generated default: scripts/render-installers.sh owns INSTALL_BASE_URL.
set -eu
INSTALL_BASE_URL='https://api.samp.team/gitcode' # rendered-install-base
base=${GITCODE_BASE_URL:-$INSTALL_BASE_URL}
base=${base%/}
repo_api=${GITCODE_GITHUB_API_URL:-https://api.github.com/repos/jaddd14/gitcode-agent/releases}
install_dir=${GITCODE_INSTALL_DIR:-${HOME:?HOME is required}/.local/bin}
fail() { printf 'Ошибка установки: %s\n' "$*" >&2; exit 1; }
command -v curl >/dev/null 2>&1 || fail 'Требуется curl.'
command -v tar >/dev/null 2>&1 || fail 'Требуется tar.'
case $(uname -s) in Linux) os=unknown-linux-gnu;; Darwin) os=apple-darwin;; *) fail 'Поддерживаются Linux и macOS.';; esac
case $(uname -m) in x86_64|amd64) arch=x86_64;; aarch64|arm64) arch=aarch64;; *) fail 'Неподдерживаемая архитектура.';; esac
target=$arch-$os
version=${GITCODE_VERSION:-}
version=${version#v}
valid_version() { case "$1" in ''|*[!0-9A-Za-z.+-]*|[!0-9]*) return 1;; esac; }
[ -z "$version" ] || valid_version "$version" || fail 'Некорректная версия.'
umask 077
work=$(mktemp -d "${TMPDIR:-/tmp}/gitcode-install.XXXXXXXX")
staged=
cleanup() { rm -rf "$work"; [ -z "$staged" ] || rm -f "$staged"; }
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
fetch() {
    case "$1" in https://*|http://127.0.0.1:*|http://localhost:*) ;; *) return 1;; esac
    curl --fail --silent --show-error --location --proto '=https,http' --proto-redir '=https' --connect-timeout 10 --max-time 180 --retry 2 "$1" -o "$2"
}
# Parse JSON without requiring Python, jq, or a non-default macOS runtime.
# Emits dotted paths and string values, rejecting ambiguous keys/escapes.
json_value() {
    awk -v wanted="$2" '
    function ws() { while (substr(s,p,1) ~ /[ \t\r\n]/ && p<=length(s)) p++ }
    function str(    out,c,e) {
        if(substr(s,p++,1)!="\"") {bad=1;return ""}
        out=""
        while(p<=length(s)) {
            c=substr(s,p++,1)
            if(c=="\"") return out
            if(c=="\\") {
                e=substr(s,p++,1)
                if(e=="/" || e=="\\" || e=="\"") c=e
                else if(e=="n" || e=="r" || e=="t" || e=="b" || e=="f") {out=out "\\" e;continue}
                else if(e=="u") {
                    c=substr(s,p,4)
                    if(length(c)!=4 || c ~ /[^0-9a-fA-F]/){bad=1;return ""}
                    p+=4;out=out "\\u" c;continue
                }
                else {bad=1;return ""}
            }
            if(c ~ /[\t\r\n]/) {bad=1;return ""}
            out=out c
        }
        bad=1; return ""
    }
    function value(path,depth,    c,k,v,sep,key,seen) {
        if(depth>32) {bad=1;return}
        ws();c=substr(s,p,1)
        if(c=="{") {
            p++;ws();if(substr(s,p,1)=="}"){p++;return}
            while(p<=length(s) && !bad) {
                ws();k=str();key=path "." k
                if(keys[key]++) {bad=1;return}
                ws();if(substr(s,p++,1)!=":"){bad=1;return}
                value(key,depth+1);ws();sep=substr(s,p++,1)
                if(sep=="}")return
                if(sep!=","){bad=1;return}
            }
        } else if(c=="[") {
            p++;ws();if(substr(s,p,1)=="]"){p++;return}
            k=0
            while(p<=length(s) && !bad){value(path "." k++,depth+1);ws();sep=substr(s,p++,1);if(sep=="]")return;if(sep!=","){bad=1;return}}
        } else if(c=="\"") {v=str();if(path==wanted){result=v;found++}}
        else {v="";while(p<=length(s) && substr(s,p,1)!~/[ \t\r\n,}\]]/)v=v substr(s,p++,1);if(v!~/^(true|false|null|-?[0-9]+([.][0-9]+)?([eE][+-]?[0-9]+)?)$/)bad=1}
    }
    {s=s $0 "\n"}
    END {p=1;value("",0);ws();if(bad || p<=length(s) || found!=1)exit 1;print result}
    ' "$1"
}
checksum_file() {
    if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d ' ' -f 1
    elif command -v shasum >/dev/null 2>&1; then shasum -a 256 "$1" | cut -d ' ' -f 1
    else fail 'Требуется sha256sum или shasum.'; fi
}
asset=gitcode-$target.tar.gz
url=
sha=
if [ -n "$version" ]; then
    url=$base/v$version/$asset
    if fetch "$base/v$version/SHA256SUMS" "$work/sums"; then
        sha=$(awk -v name="$asset" '$2==name || $2=="*"name {print $1}' "$work/sums")
        [ -n "$sha" ] || fail 'В SHA256SUMS отсутствует архив платформы.'
    fi
else
    if fetch "$base/latest.json" "$work/latest.json"; then
        version=$(json_value "$work/latest.json" .version) || fail 'Некорректный latest.json.'
        version=${version#v}
        valid_version "$version" || fail 'Некорректная версия в latest.json.'
        url=$(json_value "$work/latest.json" ".platforms.$target.url") || fail 'Платформа отсутствует в latest.json.'
        sha=$(json_value "$work/latest.json" ".platforms.$target.sha256") || fail 'Нет SHA256 в latest.json.'
    fi
fi
# Only an unavailable primary endpoint triggers fallback, never a bad checksum.
if [ -z "$sha" ]; then
    if [ -n "$version" ]; then endpoint=$repo_api/tags/v$version; else endpoint=$repo_api/latest; fi
    fetch "$endpoint" "$work/release.json" || fail 'Основной сервер и GitHub недоступны.'
    tag=$(json_value "$work/release.json" .tag_name) || fail 'Некорректный ответ GitHub.'
    released=${tag#v}
    valid_version "$released" || fail 'Некорректный тег GitHub.'
    [ -z "$version" ] || [ "$version" = "$released" ] || fail 'GitHub вернул другую версию.'
    version=$released
    # Match assets by exact name, never execute server-supplied code.
    asset_url() {
        i=0
        while name=$(json_value "$work/release.json" ".assets.$i.name"); do
            if [ "$name" = "$1" ]; then json_value "$work/release.json" ".assets.$i.browser_download_url"; return; fi
            i=$((i+1))
        done
        return 1
    }
    sums_url=$(asset_url SHA256SUMS) || fail 'В релизе нет SHA256SUMS.'
    fetch "$sums_url" "$work/sums" || fail 'Не удалось скачать SHA256SUMS.'
    if url=$(asset_url "$asset"); then :; else
        asset=gitcode-v$version-$target.tar.gz
        url=$(asset_url "$asset") || fail 'В релизе нет архива для платформы.'
    fi
    sha=$(awk -v name="$asset" '$2==name || $2=="*"name {print $1}' "$work/sums")
fi
case "$sha" in *[!0-9a-fA-F]*|'') fail 'Некорректная SHA256.';; esac
[ "${#sha}" -eq 64 ] || fail 'Некорректная длина SHA256.'
fetch "$url" "$work/archive.tar.gz" || fail 'Не удалось скачать архив.'
actual=$(checksum_file "$work/archive.tar.gz")
expected=$(printf '%s' "$sha" | tr 'A-F' 'a-f')
[ "$actual" = "$expected" ] || fail 'SHA256 не совпадает; установленный файл не изменён.'
# Extract only one explicitly named regular file; never unpack archive paths.
tar -tzf "$work/archive.tar.gz" > "$work/members" || fail 'Повреждённый архив.'
member=$(awk '$0=="gitcode" || $0=="./gitcode" {print}' "$work/members")
case "$member" in gitcode|./gitcode) ;; *) fail 'Архив должен содержать ровно один gitcode в корне.';; esac
tar -tvzf "$work/archive.tar.gz" "$member" > "$work/type" || fail 'Невозможно проверить файл в архиве.'
[ "$(cut -c 1 "$work/type")" = '-' ] || fail 'gitcode должен быть обычным файлом, не ссылкой.'
mkdir -p "$install_dir" || fail 'Не удалось создать каталог установки.'
[ ! -d "$install_dir/gitcode" ] || fail 'На месте gitcode находится каталог.'
staged=$(mktemp "$install_dir/.gitcode-install.XXXXXXXX")
tar -xOzf "$work/archive.tar.gz" "$member" > "$staged" || fail 'Не удалось извлечь gitcode.'
[ -s "$staged" ] || fail 'Пустой исполняемый файл.'
chmod 755 "$staged"
mv -f "$staged" "$install_dir/gitcode"
staged=
if [ -t 1 ] && [ -z "${NO_COLOR:-}" ]; then printf '\033[38;5;212mУстановлен gitcode %s\033[0m\n' "$version"; else printf 'Установлен gitcode %s\n' "$version"; fi
printf 'Запуск: gitcode\n'
case :${PATH:-}: in *:"$install_dir":*) ;; *) printf 'Добавьте в PATH: export PATH="%s:$PATH"\n' "$install_dir";; esac

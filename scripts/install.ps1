# Generated default: scripts/render-installers.sh owns INSTALL_BASE_URL.
& {
    $ErrorActionPreference = 'Stop'
    $INSTALL_BASE_URL = 'https://api.samp.team/gitcode' # rendered-install-base
    $base = $INSTALL_BASE_URL
    if ($env:GITCODE_BASE_URL) { $base = $env:GITCODE_BASE_URL }
    $base = $base.TrimEnd('/')
    $api = 'https://api.github.com/repos/jaddd14/gitcode-agent/releases'
    if ($env:GITCODE_GITHUB_API_URL) { $api = $env:GITCODE_GITHUB_API_URL.TrimEnd('/') }
    $dir = Join-Path $env:LOCALAPPDATA 'Programs\gitcode\bin'
    if ($env:GITCODE_INSTALL_DIR) { $dir = $env:GITCODE_INSTALL_DIR }
    $dir = [IO.Path]::GetFullPath($dir)
    $version = $env:GITCODE_VERSION
    if ($version) { $version = $version -creplace '^v', '' }
    function Assert-Version([string]$Value) {
        if ($Value -cnotmatch '^[0-9][0-9A-Za-z.+-]*$') { throw 'Некорректная версия.' }
    }
    if ($version) { Assert-Version $version }
    $arch = $env:PROCESSOR_ARCHITEW6432
    if (-not $arch) { $arch = $env:PROCESSOR_ARCHITECTURE }
    if ($arch -eq 'ARM64') {
        Write-Host 'ARM64: будет установлен x64-выпуск; требуется поддержка эмуляции x64.'
    } elseif ($arch -ne 'AMD64') { throw 'Поддерживаются Windows x64 и ARM64 с эмуляцией x64.' }
    $target = 'x86_64-pc-windows-msvc'
    $asset = "gitcode-$target.zip"
    $oldTls = [Net.ServicePointManager]::SecurityProtocol
    [Net.ServicePointManager]::SecurityProtocol = $oldTls -bor [Net.SecurityProtocolType]::Tls12
    $temp = Join-Path ([IO.Path]::GetTempPath()) ('gitcode-install-' + [Guid]::NewGuid().ToString('N'))
    $staged = $null
    $backup = $null
    function Get-Download([string]$Url, [string]$Path) {
        $uri = [Uri]$Url
        if ($uri.Scheme -ne 'https' -and -not ($uri.Scheme -eq 'http' -and $uri.IsLoopback)) {
            throw 'Загрузка разрешена только через HTTPS (HTTP допустим для локальных тестов).'
        }
        # Handle redirects ourselves: HTTPS must never redirect to plaintext HTTP.
        for ($hop = 0; $hop -lt 6; $hop++) {
            $request = [Net.HttpWebRequest]::Create($uri)
            $request.AllowAutoRedirect = $false
            $request.UserAgent = 'gitcode-installer'
            $request.Timeout = 30000
            $request.ReadWriteTimeout = 180000
            $response = $request.GetResponse()
            try {
                $status = [int]$response.StatusCode
                if ($status -ge 300 -and $status -lt 400) {
                    $next = New-Object Uri -ArgumentList $uri, $response.Headers['Location']
                    if ($next.Scheme -ne 'https') { throw 'Небезопасное перенаправление загрузки.' }
                    $uri = $next
                    continue
                }
                if ($status -ne 200) { throw "Ошибка HTTP: $status" }
                $inputStream = $response.GetResponseStream()
                $outputStream = [IO.File]::Create($Path)
                try { $inputStream.CopyTo($outputStream) }
                finally { $outputStream.Dispose(); $inputStream.Dispose() }
                return
            } finally { $response.Dispose() }
        }
        throw 'Слишком много перенаправлений.'
    }
    function Get-AssetUrl($Release, [string]$Name) {
        $matches = @($Release.assets | Where-Object { $_.name -ceq $Name })
        if ($matches.Count -ne 1) { throw "В релизе отсутствует единственный файл $Name." }
        return [string]$matches[0].browser_download_url
    }
    function Get-Sum([string]$Path, [string]$Name) {
        $sums = @(Get-Content -LiteralPath $Path | ForEach-Object {
            if ($_ -cmatch '^([0-9a-fA-F]{64})[ \t]+\*?(.+)$' -and $Matches[2] -ceq $Name) { $Matches[1] }
        })
        if ($sums.Count -ne 1) { throw 'Отсутствует единственная SHA256 для архива.' }
        return $sums[0]
    }
    try {
        [void][IO.Directory]::CreateDirectory($temp)
        $sha = $null
        $url = $null
        $primaryAvailable = $false
        if ($version) {
            try { Get-Download "$base/v$version/SHA256SUMS" "$temp\SHA256SUMS"; $primaryAvailable = $true }
            catch { Write-Host 'Основной сервер недоступен; проверяем GitHub.' }
            if ($primaryAvailable) {
                $sha = Get-Sum "$temp\SHA256SUMS" $asset
                $url = "$base/v$version/$asset"
            }
        } else {
            try { Get-Download "$base/latest.json" "$temp\latest.json"; $primaryAvailable = $true }
            catch { Write-Host 'Основной сервер недоступен; проверяем GitHub.' }
            if ($primaryAvailable) {
                $manifest = Get-Content -Raw -LiteralPath "$temp\latest.json" | ConvertFrom-Json
                $version = ([string]$manifest.version) -creplace '^v', ''
                Assert-Version $version
                $entry = $manifest.platforms.$target
                if (-not $entry) { throw 'Платформа отсутствует в latest.json.' }
                $url = [string]$entry.url
                $sha = [string]$entry.sha256
            }
        }
        if (-not $primaryAvailable) {
            $endpoint = "$api/latest"
            if ($version) { $endpoint = "$api/tags/v$version" }
            Get-Download $endpoint "$temp\release.json"
            $release = Get-Content -Raw -LiteralPath "$temp\release.json" | ConvertFrom-Json
            $released = ([string]$release.tag_name) -creplace '^v', ''
            Assert-Version $released
            if ($version -and $version -cne $released) { throw 'GitHub вернул другую версию.' }
            $version = $released
            Get-Download (Get-AssetUrl $release 'SHA256SUMS') "$temp\SHA256SUMS"
            if (@($release.assets | Where-Object { $_.name -ceq $asset }).Count -eq 0) {
                $asset = "gitcode-v$version-$target.zip"
            }
            $url = Get-AssetUrl $release $asset
            $sha = Get-Sum "$temp\SHA256SUMS" $asset
        }
        if ($sha -cnotmatch '^[0-9a-fA-F]{64}$') { throw 'Некорректная SHA256.' }
        $archive = Join-Path $temp 'archive.zip'
        Get-Download $url $archive
        if ((Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash -ine $sha) {
            throw 'SHA256 не совпадает; установленный файл не изменён.'
        }
        Add-Type -AssemblyName System.IO.Compression.FileSystem
        $zip = [IO.Compression.ZipFile]::OpenRead($archive)
        try {
            $entries = @($zip.Entries | Where-Object { $_.FullName -ceq 'gitcode.exe' })
            if ($entries.Count -ne 1 -or $entries[0].Length -eq 0) { throw 'Архив должен содержать один непустой gitcode.exe в корне.' }
            # Unix mode bits in zip attributes must not identify a symlink.
            $mode = ($entries[0].ExternalAttributes -shr 16) -band 61440
            if ($mode -ne 0 -and $mode -ne 32768) { throw 'gitcode.exe должен быть обычным файлом.' }
            [void][IO.Directory]::CreateDirectory($dir)
            $staged = Join-Path $dir ('.gitcode-install-' + [Guid]::NewGuid().ToString('N') + '.exe')
            [IO.Compression.ZipFileExtensions]::ExtractToFile($entries[0], $staged, $false)
        } finally { $zip.Dispose() }
        $destination = Join-Path $dir 'gitcode.exe'
        if ([IO.Directory]::Exists($destination)) { throw 'На месте gitcode.exe находится каталог.' }
        if ([IO.File]::Exists($destination)) {
            $backup = Join-Path $dir ('gitcode.old-' + [Guid]::NewGuid().ToString('N') + '.exe')
            [IO.File]::Move($destination, $backup)
        }
        try { [IO.File]::Move($staged, $destination); $staged = $null }
        catch {
            if ($backup -and [IO.File]::Exists($backup)) { [IO.File]::Move($backup, $destination); $backup = $null }
            throw
        }
        if ($backup) {
            try { [IO.File]::Delete($backup); $backup = $null }
            catch { Write-Host "Предыдущая запущенная версия сохранена: $backup. Удалите её после завершения процесса." }
        }
        $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
        $parts = @($userPath -split ';' | Where-Object { $_ })
        $present = @($parts | Where-Object { [Environment]::ExpandEnvironmentVariables($_).TrimEnd('\') -ieq $dir.TrimEnd('\') }).Count -gt 0
        if (-not $present) {
            [Environment]::SetEnvironmentVariable('Path', (($parts + $dir) -join ';'), 'User')
        }
        if (@($env:Path -split ';' | Where-Object { $_.TrimEnd('\') -ieq $dir.TrimEnd('\') }).Count -eq 0) {
            $env:Path = "$dir;$env:Path"
        }
        if (-not ('GitcodeInstaller.NativeEnvironment' -as [type])) {
            Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
namespace GitcodeInstaller {
    public static class NativeEnvironment {
        [DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        public static extern IntPtr SendMessageTimeout(IntPtr window, uint message, UIntPtr wparam, string lparam, uint flags, uint timeout, out UIntPtr result);
    }
}
'@
        }
        $result = [UIntPtr]::Zero
        [void][GitcodeInstaller.NativeEnvironment]::SendMessageTimeout([IntPtr]65535, 26, [UIntPtr]::Zero, 'Environment', 2, 5000, [ref]$result)
        Write-Host "Установлен gitcode $version" -ForegroundColor Magenta
        Write-Host 'Запуск: gitcode' -ForegroundColor DarkMagenta
        if (-not (Get-Command git.exe -ErrorAction SilentlyContinue)) {
            Write-Host 'Рекомендуется Git для Windows: winget install --id Git.Git -e'
        }
    } finally {
        [Net.ServicePointManager]::SecurityProtocol = $oldTls
        if ($staged -and [IO.File]::Exists($staged)) { [IO.File]::Delete($staged) }
        if ([IO.Directory]::Exists($temp)) { Remove-Item -LiteralPath $temp -Recurse -Force }
    }
}

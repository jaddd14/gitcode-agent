# Installer distribution deployment

## Status

The custom-domain endpoint is **to be enabled**. This document is an operator handoff, not evidence that the endpoint is live. No web-server configuration or service reload is performed by the build scripts. Publish only after native Linux, macOS and Windows CI, installer smoke jobs, Security and release checks pass.

## Assemble and verify

`src/update.rs` owns the `INSTALL_BASE_URL` constant. `scripts/render-installers.sh` reads it to render installer copies. An `INSTALL_BASE_URL` environment override can render a test or deployment-specific URL without editing sources.

After the release assets are published:

```sh
sh scripts/assemble-dist.sh 0.3.0
```

The assembler refuses to overwrite an existing destination, verifies all five downloaded archives against release SHA256SUMS, and writes `dist/gitcode/` with installers, latest.json and versioned archives. Select a new output directory for a second assembly. `dist/` must remain untracked.

Upload to a dedicated static directory, replacing the placeholders:

```sh
rsync -av dist/gitcode/ <user>@<host>:/srv/www/api/gitcode/
```

This command does not delete remote files. Upload immutable versioned assets first and publish latest.json last when manually deploying to an existing endpoint. Do not overwrite an existing version's archives.

## nginx example

Merge this snippet into the appropriate server block only after reviewing existing routing. The directory mapping assumes `/srv/www/api/gitcode/`:

```nginx
location /gitcode/ {
    root /srv/www/api;
    autoindex off;
    types {
        application/json json;
        application/zip zip;
        application/gzip gz;
        text/plain sh ps1;
    }
    default_type application/octet-stream;
    add_header Cache-Control "no-cache" always;
    try_files $uri =404;
}
```

This conservative cache policy also avoids caching latest.json and installer scripts. Immutable versioned archives may receive a longer policy in a separately reviewed location.

## Caddy example

```caddy
handle_path /gitcode/* {
    root * /srv/www/api/gitcode
    header Cache-Control "no-cache"
    @scripts path *.sh *.ps1
    header @scripts Content-Type "text/plain; charset=utf-8"
    file_server
}
```

Keep the host's existing TLS and routing configuration. These are examples, not instructions to replace an entire server configuration.

## Verify before advertising

```sh
curl -fsS https://api.samp.team/gitcode/latest.json
curl -fsSI https://api.samp.team/gitcode/install.sh
curl -fsSI https://api.samp.team/gitcode/install.ps1
```

Only after successful endpoint checks and a verified release advertise:

```sh
curl -fsSL https://api.samp.team/gitcode/install.sh | bash
```

```powershell
irm https://api.samp.team/gitcode/install.ps1 | iex
```

If the custom domain is unavailable, use the reviewed GitHub-hosted scripts with `GITCODE_BASE_URL` pointing to the published release assets. Do not advertise a Windows installer before its native smoke job passes. Installers and self-update require no GitHub token for public releases.

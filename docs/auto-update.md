# Atualização automática

O OpenPen procura versões novas no **GitHub Releases** do repositório
[`BrunoMoraes-Z/OpenPen`](https://github.com/BrunoMoraes-Z/OpenPen/releases). Em instalações feitas
pelo MSI, ele baixa o instalador, confere o SHA-256 e se atualiza com um clique ("Install and
restart"). Em cópias portáteis e builds de desenvolvimento, o botão "Download" abre a página da
release no navegador. O app nunca instala nada sobre esse tipo de cópia.

A lógica de rede e de instalação fica em `crates/pen-update` (sem GPUI). A integração com a UI fica
em `apps/openpen/src/update/` (`mod.rs`: modelo e fluxo; `ui.rs`: banner, diálogos).

Referência do legado: `docs/legacy-analysis/03-processo-principal-electron.md` §11. O Pen usa
`electron-updater` com provider GitHub, checa no boot e a cada 30 min, e instala só quando o
usuário pede.

## Contrato das releases

O workflow de release e o MSI (`installer/`, `docs/release.md`) seguem este contrato:

- Tag `vX.Y.Z`, igual à versão do workspace (`env!("CARGO_PKG_VERSION")`).
- Assets da release:
  - `OpenPen-X.Y.Z-x64.msi`
  - `OpenPen-X.Y.Z-x64.msi.sha256`, no formato do `sha256sum`: `<hex minúsculo>  OpenPen-X.Y.Z-x64.msi\n`
  - `OpenPen-X.Y.Z-x64-portable.zip`
- MSI por usuário (sem UAC), em `%LOCALAPPDATA%\Programs\OpenPen\`, com atualização no lugar
  (`MajorUpgrade`).
- Instalação silenciosa: `msiexec /i <msi> /qn /norestart LAUNCHAPP=1`. Com `LAUNCHAPP=1`, o MSI
  abre o app de novo no fim.

Uma release sem o `.msi` ou sem o `.sha256` (por exemplo, com os assets ainda subindo) é ignorada.

## API (`pen-update`)

```rust
// Assíncronas, sobre reqwest/rustls (o mesmo stack do pen-agent).
let u = Updater::from_env()?;                          // GitHub ou $OPENPEN_UPDATE_URL
u.check(&current, Channel::Stable).await?              // -> CheckResult
u.check_latest(&current, channel).await?               // -> Option<UpdateInfo>
u.download(&info, |p: Progress| true).await?           // -> PathBuf (MSI verificado)
pen_update::install_and_relaunch(&msi)?;               // msiexec destacado

// Atalhos bloqueantes (runtime Tokio próprio): check_latest(), download(), block_on().
```

- `CheckResult`: `Available(Box<UpdateInfo>)`, `UpToDate { latest }` ou `NoReleases`.
- `UpdateInfo`: `version`, `tag`, `title`, `notes_md`, `published_at`, `html_url`, `prerelease`,
  `msi_name`, `msi_url`, `sha_url`, `portable_url` e `size`.
- `Error`: `Offline`, `RateLimited { reset_at }`, `Http { status, message }`,
  `InvalidResponse`, `ChecksumMismatch`, `Refused`, `Cancelled`, `Unsupported` e `Io`.

### Consulta

- Canal estável: `GET /repos/BrunoMoraes-Z/OpenPen/releases/latest`. O GitHub já exclui drafts e
  prereleases desse endpoint.
- Canal beta: `GET /repos/BrunoMoraes-Z/OpenPen/releases?per_page=30`. Entram as prereleases;
  drafts nunca entram.
- Cabeçalhos: `Accept: application/vnd.github+json`, `X-GitHub-Api-Version: 2022-11-28` e
  `User-Agent: OpenPen-Updater/<versão>`. Não vai nenhum token, ID ou dado pessoal.
- Timeouts: conexão em 10 s, leitura em 30 s e 20 s no total da consulta.
- Erros tratados:
  - **404**: o repositório ainda não tem release. Vira `NoReleases`, e o app mostra "You're up
    to date (0.1.0). No releases have been published yet."
  - **403 com `x-ratelimit-remaining: 0`, ou 429**: vira `RateLimited`, com o horário de
    `x-ratelimit-reset` ou de `retry-after`. Sem autenticação, o limite é de 60 requisições por
    hora por IP.
  - **Sem rede ou timeout**: vira `Offline`. Uma falha offline não conta como checagem feita.
- Comparação por semver (`semver`): a tag aceita prefixo `v`. Só oferece versão estritamente maior
  que a atual, ou seja, nunca faz downgrade. Um build `0.2.0-rc.1` recebe a `0.2.0` final.

### Download e verificação

1. O `.sha256` é baixado primeiro, com limite de 4 KB. Se ele citar um nome de arquivo, o nome
   precisa ser o do MSI.
2. O MSI vai para `%TEMP%\OpenPen-update\<nome>.part`, com o SHA-256 calculado durante o
   download e o progresso enviado ao callback, que cancela se devolver `false`.
3. Se o tamanho difere do informado pela API, ou se o hash não confere, o arquivo é apagado e o
   download é recusado (`ChecksumMismatch`). Se tudo confere, o `.part` é renomeado para o `.msi`.
4. Hosts aceitos para download: `https://github.com`, `api.github.com` e
   `*.githubusercontent.com`, inclusive nos redirects. Com `OPENPEN_UPDATE_URL` definido, também
   o host e a porta dessa URL.

### Instalação

`install_and_relaunch(msi)` roda, como processo destacado (`DETACHED_PROCESS`), o comando:

```
%SystemRoot%\System32\msiexec.exe /i <msi> /qn /norestart LAUNCHAPP=1 /l*v <msi sem extensão>.install.log
```

Em seguida, o app fecha. A função se recusa a rodar se a cópia em execução não for uma
instalação MSI (ela refaz a detecção real e ignora o override de teste).

## Tipo de instalação

`pen_update::install::detect_install_kind(exe, probe)`:

| Tipo | Regra | Botão principal |
|---|---|---|
| `Dev` | O exe está numa pasta `target` de um workspace Cargo, ou há um `Cargo.toml` até 6 níveis acima. Essa regra tem prioridade sobre as outras. | Download (navegador) |
| `Msi` | O exe está em `%LOCALAPPDATA%\Programs\OpenPen\`, ou a pasta dele é a registrada pelo MSI (`HKCU\Software\OpenPen\InstallDir` ou `InstallLocation` da entrada "OpenPen" em `HKCU\…\Uninstall`). | Install and restart |
| `Portable` | Qualquer outro caso (por exemplo, `C:\drivers\pen` ou um zip extraído). | Download (navegador) |

Uma cópia portátil numa máquina que também tem o MSI instalado continua `Portable`, porque o MSI
atualizaria a outra pasta, não a cópia em execução.

## No app

- **Checagem automática**: roda 10 s depois de abrir, numa thread própria (fora da thread da UI),
  no máximo uma vez a cada 24 h (`lastCheck`). Se a versão encontrada foi marcada como "pular",
  a checagem automática fica em silêncio.
- **Checagem manual**: ☰ › Help › **Check for Updates…**, o menu nativo OpenPen (macOS), o botão
  do diálogo About ou "Check Now" em Settings. A resposta aparece como notificação:
  - "You're up to date (OpenPen 0.1.0)."
  - "… No releases have been published yet." (repositório sem release)
  - "GitHub's limit for update checks was reached. Try again in N minutes."
  - "Could not check for updates: GitHub could not be reached…"

  A checagem manual mostra a versão encontrada mesmo que ela tenha sido pulada.
- **Banner** (ilha flutuante no canto inferior direito, acima do controle de zoom), com o texto
  "OpenPen X.Y.Z is available" e as ações:
  - **Release notes**: diálogo com o markdown da release (`TextView::markdown`, como no chat),
    data, tamanho e link "View on GitHub".
  - **Install and restart** (MSI) ou **Download** (portátil ou dev, abre `html_url` no navegador).
  - **Later**: esconde o banner. Uma pílula "Update" fica na ilha direita da barra de abas e
    reabre o banner.
  - **Skip this version**: grava a versão em `skippedVersion`.
- **Install and restart**:
  1. Baixa o MSI, com barra de progresso, percentual, MB e "Cancel".
  2. Verifica o SHA-256.
  3. Pede para salvar os documentos pelo **mesmo fluxo de quando o app fecha**
     (`Workspace::confirm_unsaved`): "Save All", "Install Without Saving" ou "Cancel". "Save
     All" salva cada documento e abre o Save As para os que ainda não têm arquivo.
  4. Roda o `msiexec` destacado e fecha o app. O MSI reabre o app (`LAUNCHAPP=1`).

  Se o usuário cancelar o salvamento, ou um save falhar, a atualização é abortada e o banner
  volta.
- **Versão**: ☰ › Help mostra "Version 0.1.0" e **About OpenPen** (versão, tipo de instalação,
  links). O mesmo vale para o menu nativo do macOS.
- **Settings** (botão de engrenagem, ☰ › File › Settings… ou Ctrl+,): seção **Updates** com a
  versão e o tipo de instalação, "Check for updates automatically" (ligado por padrão), "Include
  beta versions (pre-releases)" (desligado por padrão), a versão pulada com "Clear", a hora da
  última checagem e "Check Now".

### Preferências

Ficam em `update-settings.json`, na pasta de dados do app (`$OPENPEN_DATA_DIR`, senão
`%APPDATA%\OpenPen`):

```json
{ "autoCheck": true, "betaChannel": false, "skippedVersion": "0.2.0", "lastCheck": 1795000000 }
```

## Privacidade

- As únicas conexões de rede são para `api.github.com` (um GET por checagem, no máximo uma
  checagem automática por dia) e para as URLs dos assets da release (`github.com`, que redireciona
  para `*.githubusercontent.com`), e só quando o usuário pede o download.
- Não há telemetria, ID de instalação, cookie nem token. O `User-Agent` é só
  `OpenPen-Updater/<versão>`. O GitHub vê o IP, como em qualquer acesso.
- **Opt-out**: Settings › Updates › desligar "Check for updates automatically". Com isso, o app
  não faz nenhuma conexão sozinho. A checagem manual continua disponível.

## Ganchos de teste (variáveis de ambiente)

| Variável | Efeito |
|---|---|
| `OPENPEN_UPDATE_URL` | Troca `https://api.github.com` por outra base (por exemplo, um servidor mock local). Os downloads desse host passam a ser aceitos. |
| `OPENPEN_UPDATE_CHECK_DELAY_MS` | Atraso da checagem automática (padrão: 10000). |
| `OPENPEN_UPDATE_INSTALL_KIND` | `msi`, `portable` ou `dev`: força o tipo **só na UI**. A instalação continua exigindo uma instalação MSI de verdade. |
| `OPENPEN_UPDATE_DRY_RUN` | `1`: `install_and_relaunch` não roda o `msiexec`. Grava a linha de comando em `<msi sem extensão>.dry-run.txt`, e o app fecha como faria de verdade. |
| `OPENPEN_DATA_DIR` | Pasta de dados isolada (preferências e throttle de 24 h). |

Uma checagem real pelo terminal (`--beta` e uma versão atual opcional):

```
cargo run -p pen-update --example check -- 0.0.1
```

### Simulação de ponta a ponta

Exemplo de servidor mock: um diretório servido por HTTP com
`repos/BrunoMoraes-Z/OpenPen/releases/latest` (JSON no formato do GitHub, com
`browser_download_url` apontando para o próprio servidor) e os assets `OpenPen-9.9.9-x64.msi` e
`.msi.sha256`. Depois:

```
set OPENPEN_UPDATE_URL=http://127.0.0.1:8765
set OPENPEN_UPDATE_CHECK_DELAY_MS=1000
set OPENPEN_DATA_DIR=%CD%\target\update-test\data
set OPENPEN_UPDATE_INSTALL_KIND=msi
set OPENPEN_UPDATE_DRY_RUN=1
target\release\openpen.exe
```

Nunca rode um `msiexec` real com um MSI de mentira.

## Testes

- `crates/pen-update`, testes unitários:
  - comparação semver e tags;
  - nomes dos assets;
  - parsing do JSON da API (fixtures em `tests/fixtures/`, no formato real do GitHub);
  - canais estável e beta, drafts, downgrade;
  - parsing e verificação do `.sha256`;
  - preferências (round-trip e tolerância a JSON ruim), throttle de 24 h e skip-version;
  - detecção do tipo de instalação com pastas temporárias;
  - linha de comando do `msiexec`;
  - hosts de download.
- `crates/pen-update/tests/flow.rs` (wiremock): check → download → verificação completo, com os
  cabeçalhos exigidos. Também cobre hash divergente (arquivo apagado), cancelamento, 404, "up to
  date", canal beta, 403/429 de rate limit, 403 comum, offline, JSON inválido e host de download
  estranho.
- `apps/openpen/src/update/`: oferta com versão pulada (manual × automática), mensagens de
  feedback, textos de erro, ganchos de teste, rótulos e formatação, e build de desenvolvimento
  sem auto-install.

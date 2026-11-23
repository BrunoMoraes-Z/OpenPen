# Releases, instalador e CI

Este documento descreve como publicar uma versão do OpenPen, o que os workflows do GitHub Actions
fazem, como o instalador MSI se comporta e como testar tudo localmente.

## Contrato (não mudar sem combinar com o auto-updater)

- **Tag** `vX.Y.Z` igual à versão do workspace (`[workspace.package] version` no `Cargo.toml` da raiz).
  O workflow falha se forem diferentes. Só versões numéricas `X.Y.Z` (exigência do MSI).
- **Assets da GitHub Release:**
  - `OpenPen-X.Y.Z-x64.msi`
  - `OpenPen-X.Y.Z-x64.msi.sha256`: `<sha256 em hex minúsculo>  OpenPen-X.Y.Z-x64.msi\n` (formato `sha256sum`, LF, sem BOM)
  - `OpenPen-X.Y.Z-x64-portable.zip`: `openpen.exe`, `openpen-cli.exe`, `skills/openpen/*`, `LICENSE` e `README.md` na raiz
- **MSI:** por usuário (sem UAC) em `%LOCALAPPDATA%\Programs\OpenPen\`; `UpgradeCode`
  `{C0C4FB78-80F8-4BA5-9B60-159597D3B9FA}` fixo para sempre; `MajorUpgrade` substitui a versão anterior no
  lugar e bloqueia downgrade; `LAUNCHAPP=1` abre o app ao final (inclusive com `/qn`).

## Como publicar uma versão

1. Atualize a versão em `Cargo.toml` (`[workspace.package] version = "X.Y.Z"`) e rode `cargo build`
   (ou `cargo check`) para atualizar o `Cargo.lock`. O workflow usa `--locked`.
2. Commit: `git commit -am "Release X.Y.Z"` e push para `main`. Espere o CI ficar verde.
3. Tag e push da tag:

   ```powershell
   git tag vX.Y.Z
   git push origin vX.Y.Z
   ```

4. O workflow **Release** compila, testa, empacota e cria a GitHub Release com notas geradas
   automaticamente (PRs e commits desde a tag anterior) precedidas de um cabeçalho curto.

Para só gerar os artefatos (sem publicar), rode o workflow manualmente (**Actions › Release › Run workflow**).
O campo `version` é opcional: vazio usa a versão do `Cargo.toml`; outro valor só muda o nome dos arquivos e a
versão do MSI (os executáveis continuam reportando a versão do `Cargo.toml`), útil para testar upgrade.
Os arquivos ficam como artefato do workflow por 30 dias.

## Workflows

### `.github/workflows/release.yml`

Dispara em push de tags `v*` e em `workflow_dispatch`. Job `build` (`windows-latest`, permissão só de leitura):

1. checkout, Rust stable (MSVC) via `dtolnay/rust-toolchain`, cache com `Swatinem/rust-cache`;
2. `scripts/version.ps1 -Expect <tag>`: falha se a tag não bater com o `Cargo.toml`;
3. `cargo build --release --locked -p openpen -p openpen-cli`;
4. `scripts/smoke-test.ps1`: `openpen-cli --version` (tem que imprimir a versão do `Cargo.toml`), `--help`,
   VERSIONINFO dos dois `.exe`, `validate --strict` e `render` (PNG e SVG) de `installer/branding.pen`;
5. instala o WiX 5.0.2 (`dotnet tool install --global wix`) e as extensões `WixToolset.UI.wixext` e
   `WixToolset.Util.wixext` na mesma versão;
6. `scripts/package.ps1`: MSI, zip e `.sha256` em `dist/`;
7. envia os três arquivos como artefato do workflow.

Job `publish` (só em tags, `ubuntu-latest`, `contents: write`): baixa o artefato, confere o `.sha256` com
`sha256sum -c` e cria a release com `gh release create --verify-tag --generate-notes`, anexando os três arquivos.

### `.github/workflows/ci.yml`

Em push para `main` e em PRs, no `windows-latest`: `cargo fmt --all --check`,
`cargo clippy --workspace --all-targets --locked -- -D warnings` e `cargo test --workspace --locked`.
O cache só é gravado a partir de `main` (PRs reaproveitam). Em falha, sobem como artefato as saídas
divergentes dos testes golden (`*.actual.*`) e os relatórios de fidelidade do import web.

O que muda no runner em relação à máquina de desenvolvimento:

- `testdata/` não existe (gitignored): os testes de corpus fazem skip.
- `PEN_SHADER_GPU=0`: o runner não tem GPU (só o adaptador de software WARP), então os shaders usam o
  caminho de CPU, determinístico. `gpu_matches_cpu` faz skip; rode-o localmente.
- O runner tem Edge: os testes de import web (`pen-import/tests/fidelity.rs`, `apps/openpen/tests/web_import.rs`,
  `apps/openpen-cli/tests/headless.rs`) rodam de verdade, contra um servidor HTTP local em `127.0.0.1` e com
  as fontes Inter embutidas. Sem navegador, fazem skip.
- Os testes com Claude real e rede (`pen-agent/tests/real.rs`) só rodam com `PEN_AGENT_REAL_CLAUDE=1`,
  `PEN_AGENT_NET_TESTS=1` ou `ANTHROPIC_API_KEY`; o CI não define nenhum.
- Nenhum teste grava no Credential Manager (`keyring`) nem em `%APPDATA%\OpenPen`: as pastas de teste ficam
  em `target/tmp` ou `%TEMP%`.

## Instalador (`installer/`)

| Arquivo | Papel |
|---|---|
| `OpenPen.wxs` | Pacote WiX 5 (per-user, features, UI, associação, atalhos) |
| `branding.pen` | Bitmaps do instalador desenhados no próprio OpenPen (referência: `assets/icon/openpen-icon.pen`) |
| `build-installer.ps1` | `-Version X.Y.Z -BinDir <pasta dos exe> [-OutDir] [-SkipValidation]` |

O `build-installer.ps1` renderiza `branding.pen` com o `openpen-cli` recém-compilado (nós `dialog`, 493×312, e
`banner`, 493×58), converte para BMP, gera o `License.rtf` a partir do `LICENSE`, lista `skills/openpen/*`, roda
`wix build` e depois `wix msi validate` (ICE). Os avisos ICE61 (`AllowSameVersionUpgrades`) e ICE91 (pacote só
por usuário) são esperados e suprimidos.

### Comportamento

- **Instalação:** por usuário, sem UAC, em `%LOCALAPPDATA%\Programs\OpenPen\` (`openpen.exe`, `openpen-cli.exe`,
  `skills\openpen\*`). Não há escolha de pasta.
- **Features (UI "Custom Setup"):**
  - *OpenPen* (obrigatória): executáveis, skills, atalho "OpenPen" no Menu Iniciar com o ícone, associação `.pen`.
  - *Desktop shortcut* (desligada por padrão; `DESKTOP_SHORTCUT=1` liga em modo silencioso).
  - *Add openpen-cli to PATH* (desligada por padrão; `ADD_TO_PATH=1`): acrescenta a pasta de instalação ao PATH
    do usuário.
- **Associação `.pen`** em `HKCU\Software\Classes`: `.pen` → ProgID `OpenPen.Design` ("OpenPen Design", ícone do
  `openpen.exe`, `"openpen.exe" "%1"`), mais `OpenWithProgids` e `Applications\openpen.exe\SupportedTypes`.
  Se o usuário escolheu outro app em "Abrir com › Sempre" (`UserChoice`), o Windows continua respeitando essa escolha.
- **Apps & Features:** nome "OpenPen", publisher "OpenPen", versão, ícone, link de ajuda e "About" para o repositório,
  link de atualização para `/releases/latest`. "Modificar" abre a escolha de features; "Reparar" está desligado.
- **Registro para o updater:** `HKCU\Software\OpenPen` com `InstallDir`, `Version` e `SkillsDir`.
- **Upgrade:** `MajorUpgrade` com `UpgradeCode` fixo. Uma versão maior remove a anterior e instala a nova na mesma
  pasta; as features escolhidas (atalho na área de trabalho, PATH) são mantidas. A mesma versão também reinstala
  (`AllowSameVersionUpgrades`), útil para builds de teste. Downgrade falha com a mensagem
  "A newer version of OpenPen is already installed…" (código 1603 em `/qn`).
- **App aberto:** `util:CloseApplication` manda `WM_CLOSE` ao `openpen.exe`, e o Restart Manager do Windows Installer
  fecha o que ainda segurar arquivos (em `/qn`, automaticamente; com UI, o diálogo de arquivos em uso pergunta ao
  usuário). Isso inclui um `openpen-cli mcp` em execução (o shim que o Claude Code mantém aberto): depois de um
  upgrade, reconecte o MCP (`/mcp` no Claude Code). O updater sempre fecha o app antes.
- **`LAUNCHAPP=1`:** abre o `openpen.exe` depois de instalar com sucesso, em qualquer nível de UI (o updater usa
  `msiexec /i OpenPen-X.Y.Z-x64.msi /qn /norestart LAUNCHAPP=1`). Na UI completa, a página final tem a caixa
  "Launch OpenPen" (marcada); ela some quando `LAUNCHAPP=1` já foi passado, para não abrir duas vezes.
- **Desinstalação:** remove arquivos, pastas vazias, atalhos, a associação, as chaves em `HKCU\Software\OpenPen` e a
  entrada do PATH. Não toca em `%APPDATA%\OpenPen` (recentes, previews, conversas do chat). Observações do Windows
  Installer: se a associação `.pen` anterior era de outro app, ela não é restaurada; e, se a feature de PATH foi
  usada, o valor `Path` do usuário volta sem a pasta do OpenPen, mas gravado como `REG_SZ` (o conteúdo não muda).

### Linha de comando

```powershell
msiexec /i OpenPen-X.Y.Z-x64.msi                                 # UI completa
msiexec /i OpenPen-X.Y.Z-x64.msi /qn /norestart                  # silencioso
msiexec /i OpenPen-X.Y.Z-x64.msi /qn /norestart LAUNCHAPP=1 ADD_TO_PATH=1 DESKTOP_SHORTCUT=1
msiexec /i OpenPen-X.Y.Z-x64.msi /qn /l*v install.log            # com log
msiexec /x OpenPen-X.Y.Z-x64.msi /qn                             # desinstalar (ou pelo ProductCode)
```

## Assinatura de código

Os binários e o MSI ainda **não são assinados**. Consequência: o SmartScreen mostra "O Windows protegeu o
computador" no primeiro uso ("Mais informações › Executar assim mesmo"), e o download pode ser marcado como
incomum pelo navegador até ganhar reputação.

Para assinar depois:

1. Obter um certificado de assinatura de código (OV/EV) ou usar um serviço como Azure Trusted Signing
   (ou SignPath, gratuito para open source).
2. Guardar as credenciais como secrets do repositório (por exemplo `SIGN_CERT_PFX_BASE64` e `SIGN_CERT_PASSWORD`,
   ou as credenciais do Azure).
3. No `release.yml`, depois do build e antes do `package.ps1`, assinar `openpen.exe` e `openpen-cli.exe`
   (`signtool sign /fd sha256 /tr http://timestamp.digicert.com /td sha256 /f cert.pfx /p ...`); depois do
   `package.ps1`, assinar o MSI e **só então** recalcular o `.sha256` (o hash precisa ser do MSI assinado).
   O jeito mais simples é adicionar um parâmetro `-Sign` aos scripts, para que CI e execução local continuem iguais.
4. Rodar a etapa de assinatura só em tags (`if: startsWith(github.ref, 'refs/tags/v')`) para que PRs de forks
   nunca vejam os secrets.

## Testar localmente

Pré-requisitos: Rust + MSVC, .NET SDK, WiX 5 e as extensões:

```powershell
dotnet tool install --global wix --version 5.0.2
wix extension add -g WixToolset.UI.wixext/5.0.2 WixToolset.Util.wixext/5.0.2
```

Gerar os mesmos arquivos do CI:

```powershell
just dist              # build release + smoke test + target\dist\{msi, sha256, zip}
just dist 0.1.1        # mesmos binários, MSI 0.1.1 (para testar upgrade)
# sem just:
cargo build --release -p openpen -p openpen-cli
./scripts/smoke-test.ps1 -BinDir target/release
./scripts/package.ps1 -BinDir target/release [-Version 0.1.1] [-OutDir dist] [-SkipMsi]
./scripts/version.ps1 -Expect v0.1.0   # a mesma verificação de tag do workflow
```

Roteiro de verificação do MSI (por usuário e reversível):

1. Antes, se você já tem `.pen` associado a outro app, exporte a chave:
   `reg export HKCU\Software\Classes\.pen pen-assoc.reg`.
2. `msiexec /i target\dist\OpenPen-0.1.0-x64.msi /qn /norestart LAUNCHAPP=1 /l*v install.log` → código 0, o app abre,
   os arquivos estão em `%LOCALAPPDATA%\Programs\OpenPen`, existe
   `%APPDATA%\Microsoft\Windows\Start Menu\Programs\OpenPen.lnk` e `HKCU\Software\Classes\.pen` aponta para
   `OpenPen.Design`. (`cmd /c assoc .pen` lê só `HKLM` e não serve para conferir; use `reg query` ou "Abrir com".)
3. `just dist 0.1.1` e `msiexec /i target\dist\OpenPen-0.1.1-x64.msi /qn /norestart LAUNCHAPP=1` com o app aberto
   → o app é fechado, a versão em `HKCU\Software\OpenPen\Version` vira 0.1.1 e só um "OpenPen" aparece em Apps.
4. Instalar de novo o 0.1.0 → 1603 com a mensagem de downgrade no log.
5. Desinstalar pelo Settings › Apps ou `msiexec /x {ProductCode} /qn` e conferir que a pasta, os atalhos e as
   chaves `HKCU\Software\OpenPen` e `HKCU\Software\Classes\OpenPen.Design` sumiram. Restaurar `pen-assoc.reg` se for o caso.

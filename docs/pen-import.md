# pen-import: importação de páginas web

`crates/pen-import` carrega uma URL num Chromium headless (Edge, Chrome ou Chromium já instalados), percorre o DOM renderizado com um crawler próprio (`src/crawl.js`) e converte o resultado em **um** frame `.pen` editável (JSON, schema 2.22, sem ids). É o equivalente ao "browser import" do Pen (ver `legacy-analysis/03-processo-principal-electron.md` §10), reescrito do zero: nenhum código ou comentário do `dom-crawl.ts` / `@ha/electron-browser-import` foi copiado.

Não há GPUI no crate. Quem usa: o diálogo do app (`apps/openpen/src/web_import`), a ferramenta MCP `import_website` (`apps/openpen-cli/src/web_import.rs`) e a CLI (`openpen-cli import-url`).

## API

```rust
pub struct ImportOptions {
    pub url: String,
    pub viewport_width: u32,          // 1440
    pub viewport_height: u32,         // 900
    pub full_page: bool,              // true = página inteira; false = só o viewport
    pub selector: Option<String>,     // só o primeiro elemento que casar
    pub wait_ms: u64,                 // 1500, espera extra depois do load
    pub browser: Option<PathBuf>,     // None = find_browser()
    pub assets_dir: Option<PathBuf>,  // pasta do .pen; None = nada é gravado (imagens viram placeholders)
    pub timeout: Duration,            // 90 s, tempo total
    pub max_nodes: usize,             // 5000 elementos do DOM
    pub infer_layout: bool,           // false; inferência de flex (ver abaixo)
    pub capture_region: bool,         // false; screenshot exato da região importada
}
pub enum ImportProgress { Launching, Loading, Scrolling, Crawling, Assets { done, total }, Converting }
pub struct ImportResult {
    pub root: Value,                  // o frame (sem ids; o chamador atribui)
    pub title: String,
    pub warnings: Vec<String>,
    pub assets_written: Vec<PathBuf>,
    pub screenshot_png: Option<Vec<u8>>, // viewport (topo da página)
    pub region_png: Option<Vec<u8>>,     // região importada inteira (com capture_region)
    pub fonts: Vec<Value>,               // FontDef {name, url, weight, style?} para o `fonts` do documento
    pub stats: ImportStats,              // nós, textos, imagens, svgs, elementos, ms por fase
}
pub async fn import_url(opts, progress) -> Result<ImportResult, ImportError>;
pub fn import_url_blocking(opts, progress) -> Result<ImportResult, ImportError>; // runtime própria numa thread
pub fn find_browser() -> Option<PathBuf>;
// ImportError: BrowserNotFound, Navigation(String), Timeout, Crawl(String), Other(String)
```

`find_browser` respeita `OPENPEN_BROWSER`; depois procura Edge (Windows), Chrome e Chromium nos caminhos padrão de cada SO (no Linux, pelo `PATH`).

## Pipeline

1. **Launch** (`browser.rs`): `--headless=new --remote-debugging-port=0` com um **perfil temporário novo** (`--user-data-dir` em `%TEMP%/openpen-import-*`), sem extensões, sync, telemetria nem first-run. A porta é lida de `DevToolsActivePort`. O guard `Browser` mata a árvore de processos (`taskkill /T` no Windows) e apaga o perfil no `Drop`, então isso acontece em todo caminho de saída: sucesso, erro, timeout ou future descartado.
2. **CDP** (`cdp.rs`): cliente mínimo sobre `tokio-tungstenite` (só `ws://127.0.0.1`, sem TLS). Sessão "flatten" (`Target.createTarget` + `attachToTarget`). Só request/response; eventos são ignorados.
3. **Load**: `Emulation.setDeviceMetricsOverride` (DPR 1), scrollbars ocultas, `Page.navigate` (erro de rede → `Navigation`), espera `readyState == complete` (≤ 30 s), injeta o crawler e espera a rede ficar quieta (sem recursos novos por 500 ms e imagens completas, limitado).
4. **Lazy load**: `loading="lazy"` vira `eager`; a página é rolada até o fim em passos de 80% do viewport (≤ 10 s) e volta ao topo; nova espera de rede; `wait_ms`.
5. **Freeze**: animações finitas vão para o estado final (`Animation.finish`), infinitas pausam, vídeos pausam, e `Animation.setPlaybackRate 0`.
6. **Screenshots**: viewport (`screenshot_png`) e, opcionalmente, a região inteira (`captureBeyondViewport`).
7. **Pseudo-elementos**: `::before`/`::after` com `content` de texto ou `url()` viram elementos reais `<openpen-pseudo>` com todo o estilo computado copiado (o original é desligado por atributo), então o crawl os mede como qualquer elemento. Marcadores de lista (`::marker` de `disc`, `circle`, `square`, `decimal`, letras, romanos) são sintetizados como texto.
8. **Crawl** (`crawl.js`): lê os `transform` computados e então os desliga (folha `transform: none !important`) para medir caixas **sem transformação**; o conversor reaplica rotação/translação. Percorre o DOM (shadow roots abertos, `<slot>`, `display: contents`, iframes same-origin como subárvore), ignora `display: none`, conteúdo sr-only e o que está fora da região (modo viewport). Coleta por elemento: retângulo em coordenadas de página, whitelist de estilos computados (só os valores diferentes do padrão), cores normalizadas para `rgb()/rgba()` (oklch, lab, color() etc. passam por um canvas), nome de componente React/Vue quando o elemento é a raiz do componente. Textos: uma linha por quebra real do navegador (retângulos de `Range` por caractere, com orçamento). Imagens: `currentSrc` + tamanho natural. SVG inline: markup serializado com estilos computados inline, `<use>` do mesmo documento e de sprites externos embutidos. Ícones por `mask-image: url(svg)`: o SVG é baixado. Elementos ilegíveis (canvas, vídeo, iframe cross-origin, checkbox/radio/range/select/progress...) viram screenshots por CDP (`Page.captureScreenshot` com clip). Limite de `max_nodes` elementos.
9. **Fontes**: `@font-face` das folhas legíveis e das cross-origin (baixadas com `fetch`), só das famílias carregadas. A família real usada por cada pilha `font-family` vem de `CSS.getPlatformFontsForNode` (ex.: `system-ui, -apple-system, "Segoe UI"` → `Segoe UI`).
10. **Assets** (`assets.rs`): baixados pelo próprio navegador (cache de memória via `Page.getResourceContent`, depois `Network.loadNetworkResource`, por fim `fetch` na página), então não há problema de CORS nem cookies do usuário. Gravados por conteúdo: `<assets_dir>/images/<site>/<sha1[0:16]>.<ext>` e `fonts/<site>/...`. PNG/JPEG/WebP ficam como estão; SVG vira PNG com resvg; GIF, AVIF, HEIC, ICO e BMP são recodificados para PNG pelo próprio Chromium (canvas). Fontes TTF/OTF/WOFF/WOFF2 são gravadas como vieram e descritas em `ImportResult::fonts`.
11. **Conversão** (`convert.rs`): ver tabela abaixo; depois simplificação (wrappers sem visual com um filho só são achatados, frames vazios sem visual somem) e a inferência opcional de layout.

## Privacidade

- O navegador roda sempre com um perfil temporário recém-criado e apagado no fim: cookies, sessões, extensões e histórico do usuário nunca são usados nem expostos à página.
- Nada é enviado a lugar nenhum além das requisições que a própria página faz (e as dos seus assets). Não há backend, telemetria nem upload.
- Flags desligam sync, telemetria, phishing check, componentes e rede em segundo plano do navegador.
- O resultado (frame, imagens, fontes) fica só no disco, na pasta do `.pen`.

## Mapeamento DOM → `.pen`

| Origem (CSS / DOM) | `.pen` |
|---|---|
| Elemento com caixa | `frame` com `layout: "none"`, `x/y` relativos ao pai, `width/height` |
| `background-color` | `fill` sólido |
| `linear-gradient` (ângulo ou `to ...`) | gradiente `linear` com `rotation`/`size`/`center` equivalentes (linha de gradiente do CSS) |
| `radial-gradient` (círculo/elipse, extents, `at`) | gradiente `radial` (`center`, `size` em frações da caixa) |
| `conic-gradient` | gradiente `angular` |
| `url()` com `background-size: cover/contain/100% 100%` | fill `image` (`cover`/`contain`/`stretch`) |
| `url()` com tamanho/posição explícitos, `no-repeat` | retângulo filho com a imagem |
| Camadas múltiplas | fills em ordem inversa (a primeira camada CSS fica por cima) |
| `border-*` | `stroke` + `strokeWidth` (uniforme ou por lado) + `strokeAlignment: inner` |
| `outline`, `box-shadow: 0 0 0 Npx` (ring) | `stroke` externo |
| `border-radius` (px, %, com redução quando sobrepõe) | `cornerRadius` (número ou 4 cantos) |
| `box-shadow` | `effect` shadow (`outer`/`inner`, offset, blur, cor) |
| `filter: blur()` / `drop-shadow()` | `effect` blur / shadow |
| `backdrop-filter: blur()` | `effect` background_blur |
| `opacity` | `opacity` |
| `overflow` ≠ `visible` | `clip: true` |
| `transform`/`rotate`/`translate` | `rotation` (anti-horária) + posição do canto; filhos no referencial local |
| `z-index` / posicionados | ordem dos filhos (z negativo, fluxo, posicionados, z positivo) |
| Texto com só texto num bloco | um `text` `fixed-width` com `\n` nas quebras do navegador, ancorado na caixa de conteúdo conforme `textAlign`, com folga do lado livre |
| Texto misturado com inline / linha única à esquerda | um `text` `auto` por linha renderizada |
| `font-family/size/weight/style`, `letter-spacing`, `line-height`, `color`, `text-transform`, `text-decoration`, `text-shadow`, `background-clip: text` | `fontFamily` (família real), `fontSize`, `fontWeight`, `fontStyle`, `letterSpacing`, `lineHeight` (multiplicador), `fill` (ou gradiente), conteúdo transformado, `underline`/`strikethrough`, `effect` |
| Texto começando com `$` | escapado (`\$...`) |
| `<a href>` | `href` nos textos, `context: "Link to ..."` |
| `<img>` / `<picture>` / poster de vídeo | `rectangle` com fill `image` (`object-fit` → `mode`); com padding/borda, frame + imagem na caixa de conteúdo |
| SVG inline / ícone `mask-image` | `path`s (usvg → geometria absoluta, fills/gradientes/strokes); paths consecutivos com a mesma pintura são fundidos; SVG com máscara, filtro, pattern, imagem ou > 300 paths vira PNG (resvg) |
| `<input>`/`<textarea>` | frame com o estilo + texto do valor ou placeholder (cor de `::placeholder`) |
| `<input type=submit/button/reset>` | frame + rótulo centralizado |
| canvas, vídeo, iframe cross-origin, controles nativos | `rectangle` com o screenshot do elemento |
| Sem `assets_dir` ou falha de download | retângulo cinza `#E5E7EB` "(placeholder)" |

Nomes: componente (React/Vue) > `aria-label`/`title`/`alt` > `role` > nome semântico (`Navigation`, `Button`, `Heading 1`, ...) > `id` > primeira classe > tag. Textos levam o próprio conteúdo como nome. `context` (campo conhecido do pen-schema) recebe, por exemplo, `"Card - div"`, `"Navigation - nav"` ou `"Link to https://..."`; a raiz recebe `"Imported from <url>"`.

### Inferência de layout (`infer_layout`)

Opcional e conservadora: só frames vindos de `display: flex` sem wrap, com filhos de tamanho fixo (sem textos, cuja largura medida pode mudar), em fluxo e sem transform, em ordem ao longo do eixo principal, com um gap uniforme e um alinhamento cruzado comum (start/center/end). O resultado (`layout`, `gap`, `padding`, `alignItems`) é simulado e só é mantido se todo filho cair a ≤ 1 px do retângulo capturado.

## CLI

```
openpen-cli import-url <URL> -o out.pen [--width 1440 --height 900] [--viewport-only]
                       [--selector CSS] [--infer-layout] [--browser PATH] [--wait 1500] [--timeout 90]
```

Cria um `.pen` novo (ids gerados, `fonts` preenchido, passado pelo `pen-schema` para validar e salvar na forma canônica) com `images/` e `fonts/` ao lado.

## Escolha do cliente CDP

Um cliente próprio de ~150 linhas sobre `tokio-tungstenite` 0.28 (`default-features = false`, `connect`). Motivos:
- usamos ~20 métodos e só request/response; `chromiumoxide` traz o protocolo inteiro gerado (compilação longa) e escolhas próprias de runtime/websocket;
- a conexão é sempre `ws://127.0.0.1`, então não há TLS: nada de rustls/ring/native-tls a mais, compila limpo no Windows;
- `tokio-tungstenite`/`tungstenite` 0.28 já estavam no cache do cargo; `resvg`/`usvg` 0.45 são os mesmos do `pen-export`.

## Fidelidade

Teste `tests/fidelity.rs`: um servidor HTTP local (std `TcpListener`, no próprio teste) serve 5 páginas fixture (`tests/fixtures`), com Inter servida como web font (`assets/fonts/inter`) e imagens geradas. Cada página é importada (1280×800, página inteira), o `.pen` é renderizado com pen-layout + pen-raster a 1x e comparado com o screenshot da mesma região. Saídas em `target/pen-import-fidelity/<fixture>/` (`.pen`, assets, `expected.png`, `actual.png`, `diff.png`) e `report.txt`. O teste é pulado se não houver navegador.

| Fixture | Conteúdo | Pixels ≤ 32 de diferença | Blocos 4×4 ≤ 24 | Diferença média |
|---|---|---|---|---|
| landing | hero com gradiente linear 135°, web font, botões com sombra/borda, stats, gradiente radial | 99,2% | 99,7% | 0,5 |
| cards | grid de cards com sombra, raio, `overflow: hidden`, `<img object-fit: cover>`, badges, texto inline misto | 98,7% | 99,7% | 0,6 |
| nav | navbar flex, logo SVG inline, ícones SVG com stroke, `::before`/`::after`, ícone `mask-image`, lista com marcadores | 99,2% | 99,7% | 0,4 |
| form | painel com sombra dupla, inputs com valor e placeholder, textarea, checkbox (screenshot), botão com ring | 99,4% | 99,5% | 0,5 |
| lazy | banner `background-image` cover com `text-shadow`, gradiente vertical, `loading=lazy` e lazy por IntersectionObserver | 99,7% | 99,9% | 0,3 |

Limiares do teste: 96% dos pixels e 97% dos blocos por fixture. Tempo por página: ~3,5 a 4 s (a maior parte é espera de rede/rolagem).

Smoke test real (`tests/smoke.rs`, `#[ignore]`, rede): `cargo test -p pen-import --test smoke -- --ignored --nocapture` (URLs trocáveis por `OPENPEN_SMOKE_URLS`). Resultados medidos:

| Página | Pixels | Blocos | Nós | Observações |
|---|---|---|---|---|
| example.com | 98,2% | 98,9% | 17 | fiel; modo escuro respeitado |
| rust-lang.org/learn | 94,7% | 93,4% | 159 | fontes WOFF2 (Alfa Slab One, Fira Sans) caem para fallback; o resto (cores, faixas, botões, ícones) bate |
| docs.python.org/3/tutorial | 97,3% | 98,7% | 627 | marcadores de lista com glifo em vez de forma; selects viram screenshot |

## Lacunas conhecidas

- **WOFF/WOFF2** são gravadas e descritas em `fonts`, mas o renderer do OpenPen ainda não carrega o `fonts` do documento nem decodifica WOFF/WOFF2: o texto cai na fonte de fallback (TTF/OTF funcionam quando carregadas).
- `.pen` não tem spread de sombra, bordas tracejadas, cor por lado da borda, `mix-blend-mode` por nó, `clip-path` nem máscaras de gradiente: são aproximados ou ignorados (com aviso).
- `scale`/`skew` em transform: o nó fica no tamanho sem escala, centrado; skew é descartado.
- Ordem de pintura é aproximada por irmãos (z-index e posicionados); stacking contexts que atravessam níveis da árvore não são reproduzidos.
- Imagens de fundo repetidas (tiles) viram uma imagem `cover`.
- Marcadores de lista usam o glifo (`•`, `◦`, `▪`) com a fonte do item, não a forma desenhada pelo Chromium.
- Iframes cross-origin, canvas, vídeo e controles nativos viram imagem (screenshot), não camadas.
- Páginas com mais de 16 000 px de altura são cortadas; acima de `max_nodes` elementos, o resto é ignorado (com aviso).
- Texto com fonte de fallback diferente da do navegador pode ficar mais largo que o original e encostar no vizinho em trechos inline (as quebras de linha, porém, são sempre as do navegador).

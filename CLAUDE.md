# OpenPen

Editor de design nativo em Rust + GPUI, compatível com o formato `.pen` do Pen (pen.dev). A UI imita a do app original. A análise do app legado está em `docs/legacy-analysis/`; leia `00-visao-geral.md` primeiro.

## Workspace

| Crate | Papel | Pode depender de |
|---|---|---|
| `crates/pen-schema` | Tipos serde do `.pen`, migrações 1.0→2.22, validação. **Round-trip sem perda** (campos desconhecidos preservados). | serde |
| `crates/pen-doc` | Grafo de cena em memória (arena + IDs), componentes/instâncias/overrides, variáveis/temas, `DocOp` + inversa (undo em blocos) | pen-schema |
| `crates/pen-fonts` | Fontes embutidas (Inter, ícones) e catálogo das fontes do sistema, carregadas sob demanda (`system`) | fontdb |
| `crates/pen-layout` | Layout flex (taffy): `fit_content` / `fill_container` / gap / padding / align; expansão de nós `script` (`ScriptRunner`) e shaders (`ShaderRenderer`) por traits | pen-doc, pen-schema, pen-fonts |
| `crates/pen-render` | Cena resolvida → `DisplayList` (agnóstica de backend, sem GPUI) | glam |
| `crates/pen-raster` | Backend CPU da `DisplayList` (tiny-skia + cosmic-text): headless, export PNG, testes golden | pen-render, pen-fonts |
| `crates/pen-export` | Export vetorial da `DisplayList`: SVG (string determinística) e PDF (svg2pdf, fontes embutidas com subset); ver `docs/pen-export.md` | pen-render, pen-raster (texto em curvas, fallbacks raster), pen-fonts, svg2pdf, pdf-writer |
| `crates/pen-codegen` | Export de código: subárvore do `SceneDoc` (+ layout) → HTML + CSS ou HTML + Tailwind v4 (flexbox, variáveis → custom properties com temas, SVG inline, assets); ver `docs/pen-codegen.md` | pen-doc, pen-layout, pen-schema, pen-render |
| `crates/pen-shader` | Shaders GLSL (WebGL 1) de fill/effect: reescrita + naga, GPU headless (wgpu) com fallback de CPU (intérprete da IR). Plugado no `Pipeline` via `ShaderRenderer`. Ver `docs/scripts-shaders.md` | pen-layout, pen-raster, naga, wgpu |
| `crates/pen-canvas` | Elemento GPUI que desenha a `DisplayList` na GPU (pan/zoom, culling, cache) | pen-render, pen-raster (fallback CPU p/ blend/blur/clips complexos), gpui-kit |
| `crates/pen-mcp` | Servidor MCP (rmcp): ferramentas, stdio, socket local (named pipe / Unix socket) e shim; documentos via trait `ToolBackend` (ver `docs/pen-mcp.md`) | rmcp, tokio, interprocess (sem pen-doc nem GPUI) |
| `crates/pen-script` | Motor do `execute` (MCP): JS em QuickJS (`rquickjs`) contra o documento, uma transação = um undo, limites, `editId`, globais por agente, streaming; `ScriptNodeRunner` dos nós `script` (`docs/scripts-shaders.md`) | pen-doc, pen-layout, pen-raster, pen-export, pen-codegen |
| `apps/openpen` | App GPUI: janela, abas, painéis, inspector, layers, toolbar | tudo |
| `apps/openpen-cli` | Headless: `validate`, `migrate`, `render` (png/svg/pdf), `export-code` (html-css/html-tailwind), `mcp` (shim stdio → app, ou `--headless FILE`) | núcleo, pen-mcp, pen-script, pen-shader, pen-export, pen-codegen, sem GPUI |

Regra: **nada de GPUI fora de `pen-canvas` e `apps/openpen`**. O núcleo precisa compilar e ser testável sem janela.

GPUI vem via `gpui-kit = "0.9.1"` (longbridge; reexporta `gpui` 0.3.x e traz componentes, tema e ícones). Para um exemplo funcionando no Windows, veja `../tracker/src/main.rs` (`gpui_kit::application().with_assets(...)`, `gpui_kit::init`, `gpui_kit::open_window`). O código-fonte da dependência fica em `~/.cargo/registry/src/*/gpui-kit-0.9.1`, `gpui-pre-0.3.12` e `gpui-component-0.9.1`. Leia a API real; não chute.

## Performance (requisito de produto)

- Abrir e migrar o `.pen` de 1,4 MB em < 100 ms (release). Pan/zoom a 60 fps (meta 120) em `testdata/user/t-process-recorder.pen`.
- Culling por viewport e índice espacial. Não reconstruir a `DisplayList` inteira por frame: invalidação por nó/subárvore.
- Evitar `clone` de subárvores, `String` em caminho quente e `serde_json::Value` no modelo de cena (só no "bag" de campos desconhecidos).
- Usar benchmarks (`cargo bench` / `criterion`) para os caminhos críticos: parse, migração, layout, build da display list.

## Testes

- `cargo test --workspace` precisa passar sempre.
- Corpus: `testdata/legacy/*.pen` (amostras do Pen, versões 2.6–2.13) e `testdata/user/*.pen` (designs reais, 2.14–2.22). Os dois estão no `.gitignore`: nunca commitar nem redistribuir. Os testes que dependem deles fazem skip se a pasta não existir.
- Golden images: `pen-raster` renderiza e compara com PNG de referência, com tolerância. Saídas divergentes viram `*.actual.png`.

## Convenções

- Identificadores e comentários de código em inglês; docs em `docs/` em pt-BR.
- `edition = 2024`, `cargo fmt` e `cargo clippy --workspace -- -D warnings` limpos.
- Referência do legado (somente leitura, proprietária, não copiar código nem assets): `legacy/extracted/` (`editor/assets/index.js` e `index-*.css` são a UI React minificada; `schema/pen.schema.json`; `skills/`).
- Nunca executar `legacy/Pen.exe`: ele reescreve configs de MCP de outros apps na inicialização.
- Doctests desabilitados (`[lib] doctest = false`): o toolchain local não tem rustdoc. Para novos crates, faça o mesmo.

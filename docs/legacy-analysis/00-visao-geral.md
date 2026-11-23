# 00 — Visão geral do Pen legado e plano para o OpenPen (Rust + GPUI)

Análise estática do **Pen v1.2.23** (pen.dev, High Agency), instalado em `legacy/`. O `app.asar` foi extraído e lido sem executar nada. Os detalhes estão em:

| Doc | Conteúdo |
|---|---|
| [01-formato-pen.md](01-formato-pen.md) | Spec do formato `.pen` (schema 2.22), migrações 1.0→2.22, componentes, variáveis/temas, libs, scripts/shaders, modelo serde proposto |
| [02-editor-e-renderizacao.md](02-editor-e-renderizacao.md) | Editor React + `pencil.wasm` (Skia), canvas, ferramentas, 132 ações/atalhos, QuickJS, shaders, export |
| [03-processo-principal-electron.md](03-processo-principal-electron.md) | Processos, protocolo IPC, ciclo de vida do documento, autosave, previews, browser import, chat DB, auth, updater |
| [04-mcp-headless-agentes.md](04-mcp-headless-agentes.md) | Servidor MCP (Go), ferramenta `execute` (JS/QuickJS), headless, agentes embutidos, geração de imagens, skills |

---

## 1. O app em uma página

```
┌──────────────────────────── Electron main (Node) ────────────────────────────┐
│ janelas/abas · menus · file IO atômico · autosave → ~/.pencil/backup          │
│ SQLite (chat) · electron-store (config) · updater (GitHub) · auth PKCE        │
│ agentes: Claude Agent SDK (MCP in-process) · Codex · Cursor · "Pi"            │
│ browser import (CDP + script injetado)  ·  previews (utility process)        │
└───────────────┬───────────────────────────── named pipe ──────────┬──────────┘
      IPC único "ipc-message" (JSON-RPC)                    mcp-server.exe (Go)
                │                                            stdio ⇄ pipe (relay)
┌───────────────▼──────────── Renderer (1 por documento) ──────────▼──────────┐
│ React 19 + Radix/Tailwind: painéis, inspector, layers, chat (tiptap)          │
│ "Managers" JS: DONO do documento, grafo de cena, layout flex, hit test,       │
│   snapping, ferramentas, undo (ações inversas em blocos), tile renderer       │
│ handlers das ferramentas MCP (execute, get_style, get_app_state, …)           │
│   ├─ QuickJS (asyncify): `execute` dos agentes, transação = 1 undo            │
│   └─ QuickJS (sync): nós `script` procedurais (2 s, 16 MiB, ≤1000 nós)        │
│ pencil.wasm (C++, ~9,5 MB): Skia Ganesh/WebGL2 + skparagraph/HarfBuzz/ICU +   │
│   PDF + codecs. Só desenha. API C de 397 funções `pencil_*`                   │
└──────────────────────────────────────────────────────────────────────────────┘
```

O que muda o plano:

1. **O formato é aberto e simples.** É JSON UTF-8 com JSON Schema e não é criptografado: o aviso de "criptografado" nas instruções do MCP só serve para os agentes não lerem o arquivo direto. Dá para ser compatível com `.pen` desde o dia 1.
2. **O C++ só desenha.** Toda a lógica do editor (cena, layout, ferramentas, undo) está em JS, então não há algoritmo escondido em binário: tudo é reimplementável.
3. **O main é fino.** O renderer é dono do documento. Num app nativo de processo único, o IPC deixa de existir: vira chamada direta entre entidades GPUI.
4. **A IA é o diferencial do produto.** O agente usa a ferramenta MCP `execute`, que roda JS em QuickJS com uma API global pequena (`Insert/Update/Copy/Move/Delete/Get/SetVariables/…`). A execução é transacional e vai por streaming, instrução a instrução, enquanto o LLM ainda escreve o código.

---

## 2. Arquitetura proposta (workspace Cargo)

Separar o núcleo da UI permite ter headless, CLI, MCP e testes sem janela, como o `@ha/headless` do legado.

```
openpen/
├─ crates/
│  ├─ pen-schema     # tipos serde do .pen + migrações 1.0→2.22 + validação (schemars)
│  ├─ pen-doc        # grafo de cena em memória, IDs, componentes/instâncias/overrides,
│  │                 #   variáveis/temas, imports de libs, enum DocOp + inversas (undo)
│  ├─ pen-layout     # flex de eixo único sobre taffy (fit_content/fill_container)
│  ├─ pen-text       # shaping/layout de texto (skia textlayout ou parley)
│  ├─ pen-render     # cena → comandos de desenho; backend Skia (skia-safe)
│  │                 #   usado por canvas, screenshot, export PNG/PDF/SVG e previews
│  ├─ pen-script     # rquickjs: nós `script` (sync) e API `execute` (async, transação)
│  ├─ pen-mcp        # servidor MCP (rmcp) + shim stdio⇄pipe; schemas compartilhados
│  ├─ pen-agent      # laço agentic próprio (Anthropic/OpenAI), skills, cursor/flash
│  └─ pen-store      # recentes, config, autosave/backup, chat (rusqlite), keyring
├─ apps/
│  ├─ openpen        # GPUI: janelas, abas, painéis, inspector, layers, chat, canvas
│  └─ openpen-cli    # headless: render/export/validate/migrate, `mcp` (shim stdio)
```

### Decisões técnicas recomendadas

| Área | Recomendação | Por quê |
|---|---|---|
| Renderer do canvas | **skia-safe** | O legado é Skia com API no estilo CanvasKit, então o port é quase 1:1. Ganhamos blur, sombras, path ops, SkSL `RuntimeEffect` (os shaders `.glsl` viram SkSL como no legado), skparagraph e PDF. Vello é mais "Rust", mas falta filtro, PDF e texto rico. |
| Integração com GPUI | **Fase 1:** Skia raster (CPU) → bitmap → `img`/`RenderImage` do GPUI, só redesenhando regiões com dano. **Fase 2:** textura GPU compartilhada. | A GPUI não expõe uma superfície GPU customizada de forma estável. O caminho CPU é simples e roda em todas as plataformas, e o tile cache do legado (pirâmide de zoom e LRU) mostra como ficar rápido. |
| UI (painéis etc.) | **GPUI** puro; `Entity<T>` + `cx.notify()` no lugar de EventEmitter + `useSyncExternalStore` | Encaixe direto com o padrão "Manager" do legado. |
| Layout | **taffy** | Mapeia `fit_content` / `fill_container` / gap / padding / align / `layoutPosition: absolute`. |
| Texto | `skia_safe::textlayout` (vem junto do Skia); avaliar **parley** depois | Bidi, fallback, eixos variáveis, ellipsis, hit test do cursor. |
| Scripts / `execute` | **rquickjs** | É o mesmo motor do legado, então skills e prompts existentes funcionam. Tem limite de memória, interrupt por deadline e async. |
| MCP | **rmcp** in-process + subcomando `openpen-cli mcp` (stdio⇄named pipe/Unix socket via `interprocess`) | Mesma topologia do legado, sem o binário Go. A instalação nos clientes (Claude Code, Codex…) deve ser **opt-in**. |
| Persistência | escrita atômica, `notify` para assets, `rusqlite` (chat), **keyring** para tokens | O legado guarda token e chave de API em texto puro: não repetir. |
| Browser import | **adiar**; depois `chromiumoxide` (CDP) | É a parte mais cara e menos central. |

---

## 3. Roteiro sugerido

| Fase | Entrega | Critério de pronto |
|---|---|---|
| **0. Formato** | `pen-schema` + migrações + `openpen-cli validate/migrate` | Os 11 `.pen` de `legacy/.../out/data` fazem round-trip (ler → escrever → JSON equivalente) |
| **1. Ver** | `pen-doc` + `pen-layout` + `pen-render` → `openpen-cli render doc.pen -o out.png` | Os exemplos renderizam visualmente próximos ao Pen (comparar com screenshots do app legado) |
| **2. Visualizador GPUI** | Janela com canvas (pan/zoom), árvore de layers, inspector somente leitura | Abre qualquer `.pen`, 60 fps no pan com o `pencil-demo.pen` (1,3 MB) |
| **3. Editor básico** | Seleção, mover/redimensionar, handles, snapping (5 px), undo/redo por blocos, frame/rect/ellipse/text, salvar e autosave | Editar e salvar sem perder dados desconhecidos (preservar campos não modelados) |
| **4. Componentes e variáveis** | Instâncias `ref`, overrides em `descendants`, slots, temas, imports de `.lib.pen` | As libs shadcn/halo/lunaris/nitro funcionam |
| **5. IA** | `pen-script` (`execute`) + `pen-mcp` → Claude Code conectando no OpenPen | Um agente externo cria uma tela via `execute`, com 1 passo de undo |
| **6. Extras** | Nós `script` e shaders, export SVG/PDF, pen tool/vetores, chat embutido, geração de imagens, browser import | Conforme prioridade |

---

## 4. Riscos e pontos em aberto

- **Canvas dentro da GPUI:** o desempenho do caminho CPU→textura precisa ser medido cedo, num spike na fase 1/2.
- **Build do skia-safe no Windows:** usar os binários pré-compilados do crate (features `gl`/`textlayout`/`pdf`/`svg`). Se dificultar, `tiny-skia` serve como fallback para o headless.
- **Lacunas da spec:**
  - não está confirmado se o texto rico (spans) chega a ser usado;
  - algumas migrações não percorrem `descendants`;
  - há divergências de defaults (`strokeAlignment`, sombras internas, `icon.weight`).
  - Validar contra arquivos gerados por uma versão recente do Pen.
- **Compatibilidade de prompts:** manter a mesma API do `execute` permite reaproveitar o conhecimento dos LLMs. Os textos das skills do Pen (`out/skills/pen-dev/*.md`) são do fornecedor: escrever os nossos a partir da spec em vez de copiá-los.
- **Assets do fornecedor** (kits `.lib.pen`, fonte `pencil.woff`, shaders e scripts de exemplo, ícones) servem só como referência de teste, não para redistribuir.

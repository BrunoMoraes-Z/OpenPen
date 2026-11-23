# Especificação de UI do OpenPen (referência: Pen v1.2.23)

Este documento descreve a interface do editor Pen para que o OpenPen (Rust + GPUI) a reproduza. Ele vem de **análise estática** de `legacy/extracted/editor/assets/index-BJi2dmbc.css` (Tailwind v4), `legacy/extracted/editor/assets/index.js` (React 19 + Base UI/Radix) e `legacy/extracted/main/*.js`. Nenhum código ou asset do legado deve ser copiado: aqui só há medidas, cores, nomes e comportamento.

Convenções:

- **[C]** confirmado no código-fonte (classe, constante ou string literal).
- **[I]** inferido (composição de classes, cálculo ou leitura de contexto). Vale conferir visualmente quando possível.
- `1 unidade Tailwind = 4 px` (`--spacing: .25rem`, rem = 16 px). Ex.: `h-7` = 28 px, `p-1` = 4 px.
- "Mod" = Ctrl no Windows/Linux e ⌘ no macOS. Nos atalhos do registro de ações, os modificadores são bits: `1=Mod`, `2=Shift`, `4=Alt`, `8=Ctrl` (`3` = Mod+Shift, `5` = Mod+Alt, `6` = Shift+Alt).
- Os hex de cores "compostas" (ex.: `foreground/6%` sobre a ilha) foram calculados em sRGB sobre o fundo indicado **[I]**. No GPUI, prefira desenhar a cor com alfa por cima do fundo real.

---

## 1. Layout geral

### 1.1 Janela (processo principal)

| Item | Valor | Fonte |
|---|---|---|
| Tamanho padrão | 1352 × 878 | `config.js` `windowBounds` [C] |
| Tamanho mínimo | 560 × 400 | `app-window.js` [C] |
| Cor de fundo da janela | escuro `#18181b`, claro `#f6f6f6` | `getWindowBackgroundColor` [C] |
| Título | `"pen.dev"` (no OpenPen: "OpenPen") | [C] |
| Frame no Windows | **frame nativo** (`frame: !IS_MAC`) com `autoHideMenuBar`. A barra de título do SO (min/max/fechar) fica acima. A tab bar de 50 px é desenhada **dentro** da área cliente. | [C] |
| Frame no macOS | sem frame, `titleBarStyle: hiddenInset`; semáforos em `x = 8 + 12 = 20`, `y = 8 + round((42 − 14)/2) = 22` | [C] |
| Tema padrão | `localStorage.theme ?? "dark"`, ou seja, **escuro** | [C] |

No OpenPen com GPUI há duas opções no Windows: (a) frame nativo, igual ao legado, ou (b) titlebar client-side de 50 px com os botões de janela à direita da ilha direita. (a) é fiel; (b) é decisão de produto.

### 1.2 Camadas e medidas (janela padrão 1352 × 878, sidebar visível)

O canvas ocupa **toda** a área cliente (inclusive atrás da tab bar). Os painéis são "ilhas" flutuantes com cantos de 20 px.

```
x=0                                                                                  x=1352
┌────────────────────────────────────────────────────────────────────────────────────────┐ y=0
│ pt=8 px=8                                                                              │
│ ┌──────────── ilha esquerda (h=42) ─────────────┐        ┌── ilha direita (h=42) ──┐  │
│ │ ☰  pen  ⌂ │ [aba ●][aba ×][aba] [+]           │  ...   │ Share 🎁 ▶ 🌐           │  │ TAB BAR h=50
│ └────────────────────────────────────────────────┘        └─────────────────────────┘  │ y=50
│                                                                                        │
│  8┌──── sidebar esquerda (ilha) ────┐8┌──┐                          ┌─ inspector ─┐8   │ y=58 (=50+8)
│   │ [🕘][Chat]        [≡][▭][◇][📚] │ │⇤ │  ilha "sidebar toggle"   │ Header/nome │    │
│   │─────────────────────────────────│ └──┘                          │─────────────│    │
│   │ Layers            (h=48)        │ ┌──┐                          │ Alignment   │    │
│   │─────────────────────────────────│ │➤ │ ilha de ferramentas      │ Position    │    │
│   │ ▸ ▭ Frame 1        (linha h=28) │ │▾ │ (botões 28×28, p=4,      │ Layout      │    │
│   │   ▸ ⊞ Header                    │ │▭ │  gap=4)                  │ Appearance  │    │
│   │      T Title                    │ │▾ │                          │ Fill        │    │
│   │                                 │ │✒ │                          │ Stroke      │    │
│   │   largura padrão 378            │ │T │                          │ ...         │    │
│   │   (min 280, max 700)            │ │# │                          │ Export      │    │
│   │                                 │ │… │                          │ w=221       │    │
│   │                                 │ └──┘                          │             │    │
│   │                                 │ ┌──┐ ilha "command palette"   │             │    │
│   │                                 │ │⋯ │                          │             │    │
│   │                                 │ └──┘     CANVAS (bg canvas)   │             │    │
│   │                                 │              ┌──────────────┐ │             │    │
│   │                                 │              │ prompt/chat  │ │   ┌──────┐  │    │
│   └─────────────────────────────────┘              │ "spotlight"  │ │   │− 100% +│ │    │ zoom: bottom 8, right 8
│ 8                                                  └──────────────┘ └─────────────┘    │ (da área do canvas)
└────────────────────────────────────────────────────────────────────────────────────────┘ y=878
```

Medidas-chave:

| Elemento | Posição / tamanho | Fonte |
|---|---|---|
| Tab bar | `DESKTOP_TAB_BAR_HEIGHT = 50`, `ISLAND_INSET = 8`, `ISLAND_HEIGHT = 42` (`8 + 42 = 50`) | constantes `C3t=8, zL=42, gI=50` [C] |
| Topo dos painéis (`F0`) | `50 + 8 = 58` px no desktop (no browser: `8 + 42 + 8`) | `F0 = vwe + 8`, `vwe = gI` no Electron [C] |
| Sidebar esquerda | `left: 8` (`ml-2`), `top: 58`, `bottom: 8` (ou `altura do spotlight + 16` quando o prompt está acoplado embaixo à esquerda) | [C] |
| Largura da sidebar | `--sidebar-width`; config `sidebarWidth` padrão **378**; o resize limita a **280–700** e o duplo clique no handle volta para **320**; o máximo também é limitado pela borda do painel de navegador | `Oj.sidebarWidth`, `S9e={min:280,max:700,default:320}` [C] |
| Handle de resize | faixa de 8 px (`w-2`) colada na borda direita da sidebar (`translate-x-full`), de `top: 58` até `bottom: 8`, cursor `ew-resize` | [C] |
| Sidebar aberta no início? | `leftPanelOpen` padrão `false`; só abre na carga se `leftPanelOpen && innerWidth ≥ 1200` | [C] |
| Animação abrir/fechar sidebar | `left` de `0` para `-(sidebar-width + 8)`, `150ms ease-out`; gradiente de 64 px (`w-16`) de `canvas-bg` atrás da ilha | [C] |
| Toolbar | coluna vertical colada à **direita da sidebar** (ou a 8 px da borda esquerda com a sidebar fechada), `padding-left: 8`, `padding-top: 58`, ilhas empilhadas com `space-y-2` (8 px) | `Mba` [C] |
| Inspector | largura fixa **221 px**, `margin-right: 8`, `margin-bottom: 8`, `margin-top: 58`. No modo normal ocupa coluna própria (o canvas não passa por baixo); nos modos "colapsado/edição de path/shader/scale" flutua (`absolute right-2`) quando sobra ≥ 56 px de altura | `Cme` [C] |
| Inspector sem seleção | **não é renderizado** (`selectionCount === 0 → null`) | `qZn` [C] |
| Zoom | ilha `absolute bottom-2 right-2` (8 px) dentro da área do canvas | `K1a` [C] |
| Prompt "spotlight" do agente | `position: fixed`, centralizado, `bottom: 16`, largura `min(L, 100% − 32px)`; L padrão **625**, min 360, max 700 (localStorage `pencil-spotlight-width`); posição alternativa `bottom-left` | [C] |
| Barra de status inferior | **não existe** | [C, por ausência] |

---

## 2. Tokens de cor

Fonte: `:root` (claro) e `.dark` (escuro) em `index-BJi2dmbc.css` [C]. O tema é alternado pela classe `dark`/`light` em `<html>`.

### 2.1 Tema claro (`:root`)

| Token | Hex | Uso principal |
|---|---|---|
| `--background` | `#fafafa` | fundo base; também é o **fundo das ilhas** no claro (`--island-bg: var(--background)`) |
| `--foreground` | `#020202` | texto principal; base dos overlays `foreground/N%` |
| `--card` | `#ffffff` | cards, menu do zoom (`bg-card`) |
| `--card-foreground` | `#18181b` | texto da sidebar (`text-card-foreground`) |
| `--popover` | `#ffffff` | menus, popovers, selects |
| `--popover-foreground` | `#18181b` | texto de menus |
| `--primary` | `#18181b` | botão primário, checkbox marcado, **fundo do tooltip** |
| `--primary-foreground` | `#fafafa` | texto sobre primary |
| `--secondary` | `#f4f4f5` | botão secundário |
| `--secondary-foreground` | `#18181b` | títulos de seção do inspector |
| `--muted` | `#f4f4f5` | fundo de `kbd` |
| `--muted-foreground` | `#71717a` | texto secundário, labels de campo, abas inativas |
| `--accent` | `#f4f4f5` | hover de item de menu/select (`bg-accent`) |
| `--accent-foreground` | `#18181b` | texto sobre accent |
| `--accent-active` | `#3b82f6` | **item destacado do menu de contexto** (texto branco), botão de ícone ativo (`/20`), seleção de texto |
| `--destructive` | `#ef4444` | ações destrutivas |
| `--destructive-foreground` | `#fafafa` | |
| `--border` | `#e4e4e7` | bordas, separadores; **aro da ilha** (`--island-rim-color`) no claro |
| `--input` | `#e4e4e7` | borda de input/checkbox; `--control-separator` |
| `--ring` | `#d4d4d8` | anel de foco (`focus-visible:ring`) |
| `--sidebar` | `#fafafa` | |
| `--sidebar-foreground` | `#18181b` | |
| `--sidebar-primary` / `-foreground` | `#18181b` / `#fafafa` | |
| `--sidebar-accent` / `-foreground` | `#f4f4f5` / `#18181b` | |
| `--sidebar-border` | `#e4e4e7` | |
| `--sidebar-ring` | `#d4d4d8` | |
| `--canvas-bg` | `#f6f6f6` | **fundo do canvas** (e da janela) |
| `--island-bg` | `= --background` → `#fafafa` | fundo das ilhas (tab bar, sidebar, toolbar, inspector, zoom) |
| `--control` | `foreground 4.5%` ≈ `#efefef` sobre a ilha [I] | fundo de inputs, selects e grupos segmentados |
| `--control-hover` | `foreground 8.5%` ≈ `#e5e5e5` [I] | hover de select, input sendo arrastado (scrub) |
| `--control-separator` | `= --input` `#e4e4e7` | divisória entre o hex e o % no Fill |
| `--select-trigger` / `-hover` | transparente | |
| `--scrollbar-thumb` / `-active` / `--scrollbar-track-hover` | `#00000038` / `#0000006b` / `#0000000d` | scrollbars |
| `--chart-1..5` | `#64748b #a1a1aa #71717a #52525b #3f3f46` | gráficos |

### 2.2 Tema escuro (`.dark`)

| Token | Hex | Uso principal |
|---|---|---|
| `--background` | `#18181b` | fundo base |
| `--foreground` | `#c8c8c8` | texto principal (cinza claro, não branco) |
| `--card` | `#141414` | cards, menu do zoom |
| `--card-foreground` | `#bebebe` | texto da sidebar |
| `--popover` | `#27272a` | menus, popovers e **fundo das ilhas** (`--island-bg: var(--popover)`) |
| `--popover-foreground` | `#f4f4f5` | |
| `--primary` | `#fafafa` | tooltip (fundo claro), checkbox marcado |
| `--primary-foreground` | `#18181b` | |
| `--secondary` | `#27272a` | |
| `--secondary-foreground` | `#828282` | (os títulos de seção usam `dark:text-foreground` → `#c8c8c8`) |
| `--muted` | `#27272a` | |
| `--muted-foreground` | `#a1a1aa` | texto secundário |
| `--accent` | `#3f3f46` | hover de item de menu/select |
| `--accent-foreground` | `#f4f4f5` | |
| `--accent-active` | `#3b82f6` | igual ao claro |
| `--destructive` | `#f87171` | |
| `--destructive-foreground` | `#18181b` | |
| `--border` | `#202020` | bordas |
| `--input` | `#3f3f46` | bordas de input; `--select-trigger: input 30%` ≈ `#2e2e32` [I] |
| `--ring` | `#52525b` | |
| `--sidebar` | `#18181b` | |
| `--sidebar-foreground` | `#f4f4f5` | |
| `--sidebar-primary` / `-foreground` | `#f4f4f5` / `#18181b` | |
| `--sidebar-accent` / `-foreground` | `#27272a` / `#f4f4f5` | |
| `--sidebar-border` | `#3f3f46` | |
| `--sidebar-ring` | `#52525b` | |
| `--canvas-bg` | `#18181b` | **fundo do canvas** |
| `--island-bg` | `= --popover` → `#27272a` | fundo das ilhas (mais claras que o canvas) |
| aro da ilha | `#a0b2cc21` (≈ `#2a2c32` sobre o canvas) [C/I] | `.dark .island-shine` |
| `--control` | `foreground 10%` ≈ `#37373a` sobre a ilha [I] | inputs/selects/segmentados |
| `--control-hover` | `foreground 14%` ≈ `#3e3e40` [I] | |
| `--control-separator` | transparente | |
| scrollbars | `#ffffff3d` / `#ffffff73` / `#ffffff0f` | |
| `--chart-1..5` | `#a1a1aa #d4d4d8 #e4e4e7 #f4f4f5 #fafafa` | |

### 2.3 Cores fixas (iguais nos dois temas)

| Nome | Hex | Uso | Fonte |
|---|---|---|---|
| `--color-light-blue` | `#3d99ff` | **cor de seleção no canvas**, contorno de foco (`focus-ring`: outline 1 px, offset 0), ponto do grid de alinhamento | [C] |
| `purple` | `#9580ff` | instância no canvas; texto/borda de campo ligado a variável (`text-purple`, `border-purple`) | [C] |
| magenta | `#d480ff` | componente (reusable) no canvas; nome de variável em campos | [C] |
| verde | `#00c48c` | nó `script` (e browser com ponteiro) no canvas | [C] |
| snap/medida | `#f24518` | guias de snap, rótulos de distância | [C] |
| cinza de legenda | `#aaaaaa` | nome do frame no canvas (sem seleção nem hover) | [C] |
| blue-500 (Tailwind v4) | `#2b7fff` | linha selecionada nos Layers (`/18%`), borda do input de renomear | [C, oklch→hex] |
| toggle ligado | `#77cc66` | switch dos itens "config-toggle" do menu de contexto | [C] |
| sticky notes | Yellow (padrão), `#FFCC98`, `#FFBEB2`, `#FDC0D7`, `#DCC5F9`, `#AEDEFF`, `#B3EBBD`, `#DBDEE1` | paleta da seção Note | [C] |

Paleta Tailwind usada diretamente (oklch → hex): zinc-50 `#fafafa`, zinc-100 `#f4f4f5`, zinc-200 `#e4e4e7`, zinc-300 `#d4d4d8`, zinc-400 `#9f9fa9`, zinc-500 `#71717b`, zinc-600 `#52525c`, zinc-700 `#3f3f46`, zinc-800 `#27272a`, zinc-900 `#18181b`, zinc-950 `#09090b`; blue-200 `#bedbff`, blue-400 `#51a2ff`, blue-700 `#1447e6`; violet-400 `#a684ff`, violet-600 `#7f22fe`; indigo-300 `#a3b3ff`, indigo-500 `#615fff`; red-500 `#fb2c36`; amber-400 `#ffb900`, amber-600 `#e17100`; orange-500 `#ff6900`.

### 2.4 Cores compostas mais usadas [I]

| Uso | Claro (sobre `#fafafa`) | Escuro (sobre `#27272a`) |
|---|---|---|
| Hover genérico (`foreground/5`; dark `/10`) | `#eeeeee` | `#37373a` |
| Hover de linha nos Layers (`foreground/6`) | `#ebebeb` | `#313133` |
| Linha selecionada (`blue-500/18`) | `#d5e4fb` | `#283750` |
| Filho de selecionado (`blue-500/8`) | `#e9f0fa` | `#272e3b` |
| Botão de ícone ativo (`accent-active/20`) | `#d4e2f9` | `#2b3953` |
| Divisória vertical da tab bar (`foreground/15`) | `#d5d5d5` | `#3f3f42` |
| Borda entre seções do inspector (`foreground/6`) | `#ebebeb` | `#313133` |
| Aba ativa (`foreground/5`; dark `/10`, sobre o canvas) | `#eaeaea` | `#2a2a2c` |

---

## 3. Tipografia

| Item | Valor | Fonte |
|---|---|---|
| Família da UI | `--font-sans`: `ui-sans-serif, -apple-system, system-ui, "Segoe UI", Helvetica, "Apple Color Emoji", Arial, sans-serif, "Segoe UI Emoji", "Segoe UI Symbol"`. **No Windows, na prática: Segoe UI.** Não usa Inter na UI HTML. | [C] |
| Mono | `"Roboto Mono", monospace` (rótulo do zoom usa `font-mono`) | [C] |
| Overlays do canvas (desenhados no Skia) | **Inter**, peso 500 | [C] |
| Suavização | `antialiased` na sidebar e no inspector | [C] |

Escala de tamanhos (com line-height):

| Classe | Tamanho | Line-height | Onde |
|---|---|---|---|
| `text-3xs` | 9 px | herdado | sublabels do inspector (`text-[9px] leading-3` = 12 px), badge de comentários |
| `text-2xs` / `text-[10px]` | 10 px | herdado | labels de checkbox e radio no inspector |
| `text-xxs` | **11 px** | 16 px (`16/11`) | **todo o inspector** (inputs, selects, títulos de seção), chips da tab bar, atalhos em menus |
| `text-xs` | **12 px** | 16 px | linhas dos Layers, abas de documento, menus de contexto, tooltips, rótulo do zoom |
| `text-[13px]` | 13 px | — | botões das abas da sidebar, título do painel ("Layers") |
| `text-sm` | 14 px | 20 px | itens de dropdown genéricos, botões padrão |
| `text-base` | 16 px | 24 px | raro na UI do editor |

Pesos: 400 (normal), **500 (medium)**: abas, títulos de seção, nomes de frame/grupo nos Layers, rótulos de tool; 600 em badges.

Texto do canvas [C]:

- Legenda do frame (nome acima de frames de topo): Inter 11 px, line 13, peso 500, cor `#aaaaaa` (vira a cor de UI do nó quando selecionado/hover, ou quando é componente/instância), espaço de 5 px entre ícone e texto.
- Rótulo de dimensões da seleção ("W x H"): Inter 12 px branco, fundo = cor de UI do nó, raio 4, padding 2/4/2/4, 6 px abaixo da seleção.
- Rótulos de medida/snap: Inter 11 px (line 12), peso 500, branco, fundo `#f24518`, raio 3, padding 1/4/1/4.

---

## 4. Raios, sombras e espaçamento

### 4.1 Raios (`--radius: .625rem` = 10 px) [C]

| Classe | Valor | Uso |
|---|---|---|
| `rounded-xs` | 2 px | |
| `rounded-sm` | `radius − 4` = **6 px** | inputs, selects, segmentados, botões de ícone do inspector, itens de menu |
| `rounded` | 4 px | fundo das linhas dos Layers, checkbox (`rounded-[4px]`) |
| `rounded-[5px]` | 5 px | botão interno do segmentado |
| `rounded-md` | `radius − 2` = **8 px** | menus, popovers, tooltips, abas da sidebar |
| `rounded-lg` | 10 px | |
| `rounded-xl` | 14 px | |
| `rounded-2xl` | 16 px | |
| `rounded-3xl` | 24 px | prompt "spotlight" (uma linha) |
| `rounded-island` | **20 px** | todas as ilhas (tab bar, sidebar, toolbar, inspector, zoom) |
| botões dentro do zoom | `island − 2` = 18 px | `.zoom-controls button` |
| `rounded-full` | pílula | abas de documento, botões da tab bar |

### 4.2 Sombras [C]

| Nome | Valor | Uso |
|---|---|---|
| `shadow-island` | **nenhuma** (`--tw-shadow: none`) | ilhas: em vez de sombra, um **aro de 1 px** (`.island-shine::before`, máscara xor), cor `--border` no claro e `#a0b2cc21` no escuro |
| `shadow-xs` | `0 1px 2px 0 #0000000d` | checkbox |
| `shadow-md` | `0 4px 6px -1px #0000001a, 0 2px 4px -2px #0000001a` | tooltip, popover |
| `shadow-lg` | `0 10px 15px -3px #0000001a, 0 4px 6px -4px #0000001a` | menus (+ `ring-1 black/5`, dark `white/10`) |
| `shadow-xl` | `0 20px 25px -5px #0000001a, 0 8px 10px -6px #0000001a` | |
| segmentado ativo (claro) | `0 0 0 .5px #00000014` | |

### 4.3 Espaçamento

Base de 4 px. Valores recorrentes: 1 px (`gap-px` nos segmentados), 2 px (gap entre abas), 4 px (padding das ilhas da toolbar e gap entre ferramentas), 6 px (gap da grade do inspector e entre abas no tablist), 7 px (padding horizontal das ilhas da tab bar), 8 px (margens das ilhas), 12 px (indentação por nível nos Layers), 14/16 px (padding lateral das seções do inspector).

### 4.4 Animações [C]

- Transição padrão: `150ms cubic-bezier(.4, 0, .2, 1)`.
- Popover/menu/tooltip: entram com `scale .95 → 1` e `opacity 0 → 1`.
- Accordion: `.25s cubic-bezier(.22, 1, .36, 1)`.
- Chat em tela cheia: `600ms cubic-bezier(0.32, 0.72, 0, 1)`.

### 4.5 Scrollbars [C]

10 px de espessura, polegar com inset de 2 px e raio total. **Só aparece com hover** no container. Cores na §2.

---

## 5. Iconografia

| Item | Valor | Fonte |
|---|---|---|
| Biblioteca | **lucide-react** (cabeçalho de licença "v1.48.0"); 221 ícones distintos no bundle | [C] |
| Ícones próprios | conjunto `pen-icon`: SVG 16×16, `fill:none`, `stroke: currentColor` | [C] |
| Espessura do traço | **1 px**, com `vector-effect: non-scaling-stroke` em **todos** os ícones lucide e pen-icon (`.lucide, .pen-icon {stroke-width:1px}`). Não use o padrão de 2 px do lucide. | [C] |
| Tamanhos | toolbar e zoom 16 px (`w-4 h-4`); tab bar 14 px (`size-3.5`), ícones dentro das abas 12 px (`size-3`); abas da sidebar 15 px; tipo de nó nos Layers 14 px; chevron dos Layers 12 px; ícone dentro de input 12 px (`size-3`); botões de ícone do inspector 16 px; segmentados 16 px | [C] |
| Cor na toolbar | `zinc-800` (`#27272a`) no claro, `zinc-100` (`#f4f4f5`) no escuro | [C] |
| Opacidade | botão de ícone do inspector: 70% (ativo: 100% em `accent-active`) | [C] |

No GPUI: o `gpui-kit` traz um conjunto de ícones próprio. Para fidelidade, use ícones lucide (licença ISC) em SVG com traço de 1 px e faça os poucos pen-icons à mão, a partir das descrições abaixo (sem copiar os paths do legado).

### 5.1 Ícone por tipo de nó (Layers e header do inspector) [C]

A ordem da função `Rke` é a prioridade:

| Condição | Ícone |
|---|---|
| componente (`reusable`) | pen-icon: losango com um "X" dentro (o mesmo do botão Variables) |
| instância (ref de outro nó) | lucide `diamond` |
| frame/grupo com layout horizontal | lucide `align-start-horizontal` |
| layout vertical | lucide `align-start-vertical` |
| tem fill de imagem | lucide `image` |
| `frame` | lucide `frame` |
| `group` | lucide `square-dashed` |
| `rectangle` | lucide `square` [C, nome resolvido por mapeamento] |
| `ellipse` | lucide `circle` |
| `text` | lucide `type` |
| `broken_ref` | lucide `triangle-alert` |
| `script` | lucide `code-xml` |
| `browser` | lucide `globe` |
| `icon`, `path`, `line`, `polygon`, `note`, `root` e desconhecidos | lucide `square` (fallback genérico) |

Nomes exibidos dos tipos (`nodeType.*`): Group, Frame, Text, Path, Rectangle, Ellipse, Line, Polygon, Note, Icon, Script, Browser, Reference (broken_ref), Root, Unknown. Sem `name`, um nó `text` mostra o próprio conteúdo.

### 5.2 Ícones das ferramentas [C]

| Ferramenta | Ícone |
|---|---|
| Move | pen-icon: seta de cursor |
| Scale | lucide `scaling` |
| Rectangle | pen-icon: quadrado 12×12 |
| Ellipse | pen-icon: círculo |
| Polygon | pen-icon: triângulo |
| Icon | pen-icon: estrela/forma orgânica de 4 pontas |
| Script | lucide `code-xml` |
| Pen | lucide `pen-tool` |
| Text | pen-icon: "T" de traço fino |
| Frame | pen-icon: "#" (duas linhas verticais e duas horizontais) |
| Sticky note | pen-icon: folha com canto dobrado e duas linhas |
| Browser | lucide `globe` |
| Hand | pen-icon: mão aberta |
| Comment | lucide `message-circle` |
| Variables | pen-icon: losango com "X" |
| Find layers | lucide `search` |
| Command palette | lucide `ellipsis` |
| Toggle sidebar | lucide `panel-left` (vira `triangle-alert` vermelho com imports quebrados) |
| Path: Select points / Cut / Done | pen-icon de cursor com bolinha / lucide `scissors` / lucide `x` |

---

## 6. Barra de título e abas (Windows)

A tab bar é uma view transparente sobre o editor, com 50 px de altura e `px-2 pt-2` (8 px). Ela é `drag` (arrasta a janela); os controles são `no-drag`. Dois grupos ficam em `justify-between`, cada um numa ilha de **42 px de altura** (`relative rounded-island island-shine bg-island`). [C]

### 6.1 Ilha esquerda (no Windows: `pr-[7px] pl-1`; no mac: `px-[7px]` e um espaçador de 71 px para os semáforos)

Itens da esquerda para a direita [C]:

1. **Menu da aplicação** (só fora do mac): botão `size-7` (28 px), redondo, ícone lucide `menu` de 14 px, `mr-0.5`. Abre o menu da app como popup nativo, ancorado embaixo (+4 px).
2. **Menu "pen.dev"**: botão `h-7 px-1` redondo com o wordmark (imagem com 16 px de altura). No OpenPen, use um wordmark próprio. O menu tem: links (X, Discord, YouTube, Website), "Toggle Light/Dark mode", "Show/Hide UI" e "'pen' CLI docs".
3. **Home**: `size-7`, redondo, ícone `house` de 14 px, `ml-[6px]`. Ativo (Home aberta): `bg-foreground/5` (dark `/10`) + `ring-1 ring-inset ring-border` (dark `ring-input`). Inativo: `text-muted-foreground`, e no hover `text-foreground` + `bg-foreground/5`.
4. **Divisória**: `mx-1.5` (6 px de cada lado), `h-4 w-px`, `bg-foreground/15`.
5. **Tablist**: `gap-[6px]`, rolagem horizontal sem scrollbar (a roda vertical rola na horizontal). As abas ficam num grid com `auto-cols minmax(96px, 1fr)` e `gap 2px`.
6. **Botão "+"**: `h-7 min-w-7` redondo, ícone `plus` de 14 px, `text-muted-foreground`. Sem abas, mostra o rótulo "New" (`pl-2 pr-2.5`, `gap-1.5`). Tooltip: "New File (Ctrl⇧N)". O clique abre o menu nativo "nova aba" (Novo arquivo + recentes).

### 6.2 Aba de documento [C]

- Contêiner: `h-7` (28 px), `max-w-50` (**200 px**), `min-w-0`, `rounded-full`, `pl-0.5 pr-3`, `gap-1`, `text-xs` (12 px) **medium**.
- Ativa: fundo `bg-foreground/5` (dark `/10`), texto `foreground`. Inativa: `muted-foreground`; no hover, `foreground` + fundo `foreground/5`.
- Divisória entre abas: pseudo-elemento de 1×16 px, `foreground/15`, a `-1.5px` da borda esquerda. Só aparece entre duas abas inativas e some no hover.
- Slot do ícone à esquerda: `size-6` (24 px) redondo, ícone de 12 px: `folder` (menu "Workspace folder"; na aba inativa com 50% de opacidade, 100% no hover) ou `file` (documentos *bundled*).
- Nome: `truncate`. Sem ícone de status, o fim do texto tem máscara de fade (28 px) que some no hover.
- Status à direita (`ml-1.5`, 12 px, `muted-foreground`, some no hover): **dirty** = ponto de 8 px (`size-2 rounded-full bg-current`), tooltip "Edited"; somente leitura = `lock`; recuperado = `triangle-alert` âmbar (`#e17100`, dark `#ffb900`). Rascunhos gerenciados não mostram status.
- Fechar: botão `size-6` redondo, `absolute right-0.5`, ícone `x` de 12 px, invisível até o hover da aba. Tooltip: "Close Tab (Ctrl+W)".
- Interações: clique ativa; **clique do meio fecha**; duplo clique renomeia (exceto somente leitura); botão direito abre o menu nativo da aba; arrastar reordena (a aba arrastada ganha `z-10` e fundo `island + 5%/10% de foreground`).

### 6.3 Ilha direita (documento ativo), `gap-px px-[7px]` [C]

Os botões são `h-7 min-w-7 rounded-full text-xs`, com ícones de 14 px e hover `foreground/5` (aberto: `data-open` com o mesmo fundo). Ordem:

1. Chip "Add to {IDE}" (com uma IDE detectada): pílula `h-7` com borda, `text-xxs` e botão "x" de dispensar.
2. Badge "Offline": `h-5`, `bg-orange-500`, texto branco de 11 px.
3. "Sign In" (sem login): `border px-3`.
4. Conta: avatar com inicial e plano, seguido de divisória `mx-1.5 h-4 w-px`.
5. **Share**: ícone `share` + texto "Share" (`px-2.5`). Durante o upload: spinner + "Uploading NN%".
6. **Goodies**: ícone `gift`.
7. **Settings** (só sem conta): ícone `settings` (Ctrl+,), com ponto âmbar quando o perfil está incompleto.
8. **Present**: ícone `play` (Ctrl+↵).
9. **Browser view**: ícone `globe`, tooltip "Open a webpage and reproduce it on the canvas".

Na Home, a ilha direita mostra só Account ou Settings.

### 6.4 Menu nativo da aba (`app-window.js`) [C]

Rename… · Show in Folder (mac: Reveal in Finder) · Pop Out to Window · Close Tab · Close Other Tabs (numa janela destacada: Close Window).

---

## 7. Toolbar (ferramentas)

**Posição**: vertical, à **esquerda** do canvas, logo à direita da sidebar (ou da borda da janela), começando em `y = 58`. **Não** fica embaixo nem no topo. [C]

Estrutura (de cima para baixo, `space-y-2` = 8 px entre ilhas) [C]:

1. **Ilha "Sidebar"**: botão que abre/fecha a sidebar (ícone `panel-left`). A ilha tem `p-1`.
2. **Ilha de ferramentas**: `flex-col gap-1 p-1` (4 px). Largura total: 28 + 8 = **36 px**.
3. **Ilha "Command palette"**: botão `ellipsis` (Ctrl+P).

Botão de ferramenta: `w-7 h-7` (**28 × 28**), raio 6 px, ícone de 16 px. Inativo: `ghost` (hover `foreground/5`, dark `/10`). **Ativo**: variante `outline` (`border` + `bg-background`; dark: `bg-input/30 border-input`). Tooltip à **direita** com o atalho. [C]

Grupos com dropdown: abaixo do botão principal fica um botão de **28 × 12** (`w-7 h-3`) com `chevron-down` de 12 px (escala 75%). O menu abre à direita (`sideOffset 10`, `align start`, `min-w-8rem`). Cada item tem check, ícone de 16 px, nome e atalho à direita (`text-xxs tracking-widest muted`). O botão principal do grupo mostra a ferramenta ativa ou a última escolhida. [C]

Ordem e atalhos [C]:

| # | Entrada | Atalho | Observação |
|---|---|---|---|
| 1 | Grupo **Move Tools**: Move / Scale | V / K | Move é a única disponível em somente leitura |
| 2 | Grupo **Primitives**: Rectangle / Ellipse / Polygon / Icon / Script + ações "Import Image or SVG…", "Import Figma" | R / O / — / — / — | |
| 3 | Pen | P | |
| 4 | Text | T | |
| 5 | Frame | F (e A, segundo o registro de ações) | |
| 6 | Sticky Note | N | |
| 7 | Browser | — | só no desktop |
| 8 | Hand | H (ou segurar Space) | |
| 9 | Comment | C | badge de não lidos: `min-w-3.5`, `text-[9px]` semibold, fundo `zinc-900`/branco |
| 10 | Variables | — | abre o painel de variáveis |
| 11 | Find layers | Ctrl+F | |

Sub-toolbar de edição de path: uma ilha à direita da ilha de ferramentas (`left-full ml-1.5`), com Pen (P), Select points (V), Cut (X), uma divisória de `h-px w-4` e Done (↵). [C]

---

## 8. Painel esquerdo (sidebar)

Ilha `rounded-island island-shine bg-island`, texto `card-foreground`. [C]

### 8.1 Abas da sidebar [C]

- Linha: `px-3 pt-3 pb-0.5` (12/12/2), `gap-1`. Grupo primário à esquerda (**History**, só ícone `messages-circle`; **Chat**, ícone `message-circle`), espaçador, grupo à direita: **Layers** (`layers`), **Slides** (`presentation`), **Components** (`component`), **Libraries** (`library`; vira `triangle-alert` vermelho com import quebrado). Dentro de cada grupo, `gap-px`.
- Botão: `h-[29px] min-w-[29px]`, `rounded-md`, `text-[13px] font-medium`, padding horizontal de 7 px (primários: 8 px), ícone de 15 px. Selecionado: `bg-black/5` (dark `bg-white/10`) e texto `foreground`. Não selecionado: `muted-foreground`, com hover igual ao selecionado.
- As abas da direita são só ícone, com tooltip embaixo. O rótulo "Chat" aparece quando a sidebar tem ≥ 280 px (aba fixada) e o das demais abas primárias com ≥ 340 px. Abaixo de 240 px: botões com min-w 25 e padding 5.
- Sem agente disponível, o Chat some e a aba padrão é Layers.

### 8.2 Cabeçalho do painel (Layers/Slides/Components/Libraries) [C]

`h-12` (**48 px**), `pb-1`, `pl-2.5` (`pl-4` com ≥ 240 px), `pr-3`, `border-b` `zinc-200` (dark `zinc-700/50`). Título `text-[13px] font-medium`, `foreground/80` (dark `foreground`).

### 8.3 Árvore de Layers [C]

| Propriedade | Valor |
|---|---|
| Altura da linha | **28 px** (`--layer-row-height`, `EDe = 28`) |
| Virtualização | `estimateSize 28`, `overscan 12`, `paddingStart 6`, `paddingEnd 4` |
| Indentação | `level × 12 + 10` px de padding à esquerda (`u0t = 12`) |
| Fundo de estado (pseudo `::before`) | inset de **6 px** em cada lado (`left-[6px]`, `w-[calc(100%-12px)]`), raio 4 px. Linhas selecionadas contíguas se fundem (o raio de cima/baixo some). |
| Chevron | slot de 16 px (`w-4`), `chevron-right` de 12 px em `foreground/50`, gira 90° quando expandido. Só aparece em frame/grupo com filhos; os outros nós recebem um espaçador de 16 px. |
| Ícone de tipo | slot com `px-1`, ícone de 14 px (§5.1) |
| Nome | `text-xs` (12 px), `px-1`, altura e line-height de 28 px, `whitespace-nowrap`. Frames/grupos (pastas): **font-medium**. |
| Cor do texto | normal `foreground`; selecionado `blue-700` `#1447e6` (dark `blue-200` `#bedbff`); **componente** `violet-600` `#7f22fe` (dark `violet-400` `#a684ff`); **instância** `indigo-500` `#615fff` (dark `indigo-300` `#a3b3ff`). A cor de componente/instância prevalece sobre a de seleção. |
| Hover | `foreground/6` (≈ `#ebebeb` / `#313133`); a linha hover também destaca o nó no canvas |
| Selecionado | `blue-500/18` (≈ `#d5e4fb` / `#283750`) |
| Descendente de selecionado | `blue-500/8` (≈ `#e9f0fa` / `#272e3b`) |
| Alvo de drop | `::before` com borda 1 px `blue-500` |
| Oculto (`enabled:false`) | conteúdo com **opacidade 40%**, botão de olho sempre visível |
| Botão de visibilidade | `sticky right-0`, largura `28 + 12 = 40` px, `px-1.5`, `rounded-r`, fundo `bg-island` com o mesmo overlay da linha. Botão interno de 28×28 com ícone `eye`/`eye-off` de 16 px em `foreground/70` (hover `foreground`; roxo quando ligado a variável). Aparece no **hover**, com o menu aberto, ou sempre quando o nó está oculto. Tooltip "Hide"/"Show". |
| Lock | **não encontrado** (não há toggle de lock na linha) |
| Seleção | clique seleciona; Ctrl/Cmd+clique alterna; Shift+clique seleciona intervalo |
| Renomear | **duplo clique** abre um input inline com o texto todo selecionado: `bg-background`, `rounded`, borda 1 px `blue-500`, `px-1`, `text-xs`, altura da linha. Enter confirma, Esc cancela, blur confirma. |
| Drag & drop | reordena e reparenta; só frames/grupos aceitam filhos (`canAcceptChildren`) |
| Menu de contexto | o mesmo do canvas (§11.1); o botão direito numa linha não selecionada seleciona o nó antes |
| Vazio | "Empty canvas." / "Draw something. Add frames or other objects to the canvas to see them here." |

Busca (Ctrl+F, "Find layers"): placeholder "Find layers", dica "Searches names, text, and IDs", contador "{current} of {total}" / "{count} matches", vazio "No layers match “{query}”". [C]

---

## 9. Inspector (painel de propriedades)

Ilha de **221 px** de largura com scroll vertical (scrollbar de 10 px). Todo o conteúdo usa `text-xxs` (11 px). Com seleção vazia o painel não existe. [C]

### 9.1 Primitivas [C]

| Primitiva | Especificação |
|---|---|
| **Seção** | `grid gap-1.5` (6 px), `pt-2 pb-2` (8 px; `pb-[15px]` quando há conteúdo além do header), `pl-4 pr-3.5` (16/14 px), `border-t` `foreground/6%`. Área útil: 221 − 30 = **191 px**; 2 colunas de **≈ 92.5 px** com gap de 6. |
| **Header da seção** | `h-[22px]`, `justify-between`. Título `text-xxs font-medium`, `secondary-foreground` (dark `foreground`), truncado. Ações à direita. |
| **Sublabel** | `text-[9px]`, line-height 12 px, `muted-foreground`, `mb-1` |
| **Grade** | `grid grid-cols-2 gap-1.5` |
| **Input** | **h-6 (24 px)**, `text-xxs`, `rounded-sm` (6 px), `px-2`, fundo `bg-control` (`bg-control-hover` durante o scrub), `tabular-nums`, sem borda. Foco: outline 1 px `#3d99ff`. Desabilitado: 50%. Valor misto: "Mixed". Números com até 2 casas decimais + sufixo ("°", "%", "px"). |
| Label/ícone dentro do input | slot absoluto de **24 px** (`w-6`) à esquerda (o input ganha `pl-6`), `muted-foreground`, ícone de 12 px **ou letra** (`X`, `Y`, `W`, `H`, `%`) em 16×16 font-medium. Arrastar o slot faz **scrub** (padrão: 1 unidade a cada 2 px; Shift ×10). Setas ↑/↓ = ±step (Shift ×10). Enter confirma e sai do foco; Esc cancela. Com variáveis, aparece no hover um botão de 20 px (`w-5`) à direita para ligar uma variável. |
| **Select** | trigger `h-6`, `bg-control`, hover `bg-control-hover`, `pl-2 pr-1`, `gap-1`, chevron com 70% de opacidade. Itens: `min-h-7` (28 px), `py-1 pl-2 pr-8`, `text-xxs`, `rounded-sm`, highlight `bg-accent`, check de 16 px à direita. |
| **Segmentado** (`Nv`/`Sd`) | contêiner `flex gap-px p-px rounded-sm bg-control`. Botão `h-[22px] flex-1 rounded-[5px]`, ícone de 16 px. Ativo: `bg-white` + `font-medium` + sombra `0 0 0 .5px #00000014` (dark `foreground/16%` ≈ `#4e4e51`, sem sombra). Inativo: 70% de opacidade, 100% no hover. |
| **Grupo de botões de ação** (`N0t`) | `flex h-6 rounded-sm bg-control`. Botões `flex-1 h-full`, primeiro/último com raio. Hover `foreground/8` (dark `/10`), active `foreground/15` (dark `/20`). |
| **Botão de ícone** (`fl`) | `size-6` (24 px), `rounded-sm`, ícone de 16 px a 70%. Ativo: `bg-accent-active/20` e ícone em `accent-active`. Desabilitado: 50%. |
| **Checkbox** | 16 px, raio 4, borda `input` (dark `bg-input/30`), marcado `bg-primary`. Label `text-[10px]` normal, `ml-1`. |
| **Radio** | 14 px redondo, borda `foreground/30`; marcado: preenchido `foreground` com miolo `background` de 2 px. |
| **Ações de linha** | `ml-1 h-6` à direita da linha (olho + "−") |

### 9.2 Header (sempre o primeiro) [C]

`py-2 pl-3 pr-3.5`. Um nó: campo de nome **transparente**, com o ícone de tipo de 14 px (clicar no ícone copia o nome), e roxo quando é componente/instância. Vários nós: ícone `copy` + "{n} selected". À direita, botão de **colapsar o painel** (ícones `panel-right` / `panel-right-open`). Colapsado, o inspector mostra só o header.

### 9.3 Ordem das seções (modo normal) [C]

Cada seção só aparece quando disponível para a seleção:

1. **Header**
2. **Component**: Create Component / Detach Component / Detach instance / Make slot / Remove slot
3. **Waiting generation** (geração de imagem pendente)
4. **Slot**
5. **Context**: textarea "Context information"
6. **Alignment** (some em somente leitura)
7. **Position**
8. **Layout** (título vira **"Flex Layout"** com layout ativo)
9. **Appearance**
10. **Fill**
11. **Stroke**
12. **Typography**
13. **Icon**
14. **Note**
15. **Script**
16. **Browser**
17. **Effects**
18. **Theme**
19. **Metadata**
20. **Code**
21. **Export**

Outros modos: edição de path = Header, **Vector**, Fill, Stroke. Edição de shader = Header, ShaderEdit. Ferramenta Scale = Header, Component, Slot, Context, **Scale**.

### 9.4 Conteúdo das seções principais [C]

**Alignment**: dois grupos de ação lado a lado (`gap-1.5`), 3 botões cada: [Align left (Alt+A), Align center (Alt+H), Align right (Alt+D)] e [Align top (Alt+W), Align middle (Alt+V), Align bottom (Alt+S)]. Ícones próprios de 16 px.

**Position**: grade 2×2. `X` | `Y` (letras), **Rotation** (ícone `angle`, sufixo "°", exibe o valor com sinal invertido, Shift ×15) | grupo [Rotate 90° right, Flip horizontal (Shift+H, ícone `triangles-centerline-dashed-horizontal`), Flip vertical (Shift+V)]. Dentro de um pai com flex, aparece também o checkbox **"Absolute Position"**.

**Layout**:

- Ação no header: Add flex layout (Shift+A) / Remove flex layout (Shift+Alt+A), como botão de ícone ativo/inativo.
- Segmentado de direção: **Freeform | Vertical | Horizontal**.
- Com direção ≠ Freeform, uma grade de 2 colunas:
  - Coluna 1, sublabel "Alignment": grade 3×3 sobre `bg-control rounded-sm py-0.5`, `gap-1`, células de **26 × 20** (`w-6.5 h-5`). Ponto inativo de 2×2 px em `currentColor`; ponto selecionado de 8×8 px, raio 2, **`#3d99ff`**. Em Space Between/Around, as células da linha/coluna principal viram barras (0.5×2.5 / 0.5×1.5 ou o inverso). Tooltip com o nome da célula ("Top Left", …).
  - Coluna 2, sublabel "Gap": três radios `h-6 text-[10px]`: (●) campo numérico de gap (ícone `separator-vertical`) · "Space Between" · "Space Around".
- **Padding** (só com flex): 2 campos (horizontal e vertical; ícones `align-horizontal/vertical-space-around`) ou 4 campos (`align-horizontal-justify-start/end`, `align-vertical-justify-start/end`). O botão de ícone à direita alterna os modos: "Edit sides individually" / "Edit sides in pairs".
- **Dimensions**: `W` | `H` (letras; desabilitados conforme o "Resizing" de texto). Abaixo, os checkboxes **"Fill Width" | "Fill Height"** (`fill_container`). Se o próprio nó tem flex, também **"Hug Width" | "Hug Height"** (`fit_content`). Desmarcar volta a `Fixed`.
- **Resizing** (texto): segmentado Auto width (`arrow-right-from-line`) | Auto height (`text-align-justify`) | Fixed size (`scan-text`).

**Appearance**: no header, botão de olho (visibilidade). Grade: **Opacity** (0–100 %, scrub de 10 px por passo) | **Corner radius** (ícone `scan`). O botão "Edit corners individually" (`scan`) abre 4 campos. Subseção "Ellipse": start angle °, sweep ° (−360..360), inner radius %. Subseção "Polygon": count (3..60, ícone `pentagon`). Checkbox **"Clip Content"** (frames).

**Fill**: header "Fill" + botão "+" (Add fill). A primeira cor nova é `#d9d9d9` opaca; as seguintes, `#00000033`. Cada linha: [swatch de 16 px, raio 4, + hex no mesmo input, canto esquerdo arredondado] [campo % de **48 px** (`w-12`) com "%" à direita, separado por `border-l control-separator`] [olho] [− "Remove fill"]. Linhas reordenáveis por arraste (com grip) e copiáveis/coláveis. Clicar no swatch abre o editor (abas Color / Gradient [Linear, Radial, Angular, Mesh] / Image [Stretch: Fill, Fit] / Shader). Fill desabilitado: 40%. Tipos não sólidos mostram o nome ("Linear", "Image", …) num input somente leitura.

**Stroke**: header + "+" (a primeira cor nova é `#000000`). Linhas iguais às do Fill. Depois, grade 2 colunas: "Position" (select **Center** (padrão) / Inside / Outside) | "Weight" (`W`). Botão "Edit sides individually" abre 4 pesos. "Join" (Miter / Round / Bevel, com prévia desenhada) | "Cap" (None / Round / Square).

**Typography**: seletor de família (popover de **240 px**, busca "Search fonts...", "All Fonts" / "Custom Fonts"); aviso "“{font}” font not found". Grade: Weight (select; Thin…Black + itálicos) | Font size. "Line height" (%, aceita "Auto") | "Letter spacing" (px). "Horizontal" (segmentado Left/Center/Right/Justify, atalhos Mod+Alt+L/T/R/J; ícones `text-align-start/center/end/justify`) | "Vertical" (Top/Middle/Bottom; `arrow-up-to-line`, centro próprio, `arrow-down-to-line`).

**Effects**: header + "+" em dropdown (`min-w-40`, `text-xxs`): Drop Shadow, Inner Shadow, Layer Blur, Background Blur, Shader. O editor de efeito abre num popover de **264 px**, com cabeçalho `p-3 border-b` e corpo `p-3`.

**Export**: grade de Scale (1x/2x/3x; desabilitado em PDF/SVG) | formato (PNG, JPEG, WEBP, PDF, SVG). "Quality" para JPEG (High 95 / Medium 75 / Low 50) e WEBP (Lossless 100 / Medium 75 / Low 50). Em PDF, a dica é "Layers will be exported as pages in a single PDF…". Botão primário de largura total: "Export layer" / "Export layers (n)". As escolhas persistem em `localStorage` (`export-scale`, `export-type`, `export-quality`).

**Icon**: picker de ícone; fonte (Material Symbols Outlined/Rounded/Sharp, feather, lucide, phosphor); slider "Weight" de 100–700 em passos de 100 (para fontes variáveis).

**Note**: 8 swatches redondos de 20 px em grade de 8 colunas + campo "Color".

### 9.5 Valores ligados a variáveis [C]

Texto em roxo (`#9580ff`). Campos de cor mostram o nome da variável. No menu de variáveis, o nome aparece em `#D480FF`.

---

## 10. Controle e menu de zoom

Ilha `zoom-controls absolute bottom-2 right-2`, `flex gap-1 p-0.5`, raio 20 (botões com raio 18). [C]

- [−] botão de 28×28, ícone `minus` de 16 px · [rótulo] botão `w-11` (44 px), `py-1`, `font-mono text-xs`, `rounded`, hover `bg-accent`, mostra `round(zoom × 100)%` · [+] botão de 28×28, ícone `plus`.
- −/+ pulam para a **potência de 2** anterior/seguinte (`2^(floor(log2 z) + 1)` / `2^(ceil(log2 z) − 1)`). Limites do zoom: **0.02 a 256**.
- O menu abre ao clicar no rótulo: lado **top**, `align center`, `sideOffset 8`, fundo `bg-card`, itens `text-xs`, atalhos como `kbd` (`min-w-6`, `border`, `bg-muted`, `rounded` 4, `px-1.5 py-0.5`, `text-xs` medium).

| Item | Atalho |
|---|---|
| Zoom in | Ctrl+= (e Num+) |
| Zoom out | Ctrl+− (e Num−) |
| Zoom to fit | Shift+1 |
| Zoom to selection | Shift+2 |
| Zoom to 50% | — |
| Zoom to 100% | Ctrl+0 |
| Zoom to 200% | — |

---

## 11. Menus de contexto e menu da aplicação

### 11.1 Menu de contexto do canvas (e da lista de Layers) [C]

Estilo do contêiner: `bg-popover`, `min-w-[10rem]` (160 px), `rounded-md` (8), `p-1`, `shadow-lg`, `ring-1 black/5` (dark `white/10`).

Estilo do item: `px-2 py-1.5` (8/6), `text-xs`, `rounded-sm`, `gap-4`, rótulo à esquerda e atalho à direita (`text-xxs tracking-widest muted-foreground`, juntado sem "+" no mac e com "+" no Windows). **Destacado: fundo `accent-active` `#3b82f6` e texto branco** (o atalho fica `white/70`). Destrutivo: texto `destructive`, destacado com fundo `destructive`. Separador: `h-px bg-border -mx-1 my-1`. Submenu: `chevron-right` de 14 px, `sideOffset 8`.

Itens, nesta ordem e só quando disponíveis (separadores entre os blocos):

1. Copy (Mod+C) · **Copy as ▸** (Copy as HTML + CSS, Copy as HTML + Tailwind, Copy as JSON, Copy as PNG Mod+Shift+C, Copy as SVG) · **Paste** (ou "Paste here" quando aberto no canvas) · Paste to replace (Mod+Shift+R)
2. Present browser
3. Bring to Front (`]`) · Send to Back (`[`)
4. Group selection (Mod+G) · Frame selection (Mod+Alt+G) · Ungroup (Mod+Backspace) · Add flex layout (Shift+A) · Remove flex layout (Shift+Alt+A)
5. Create Component (Mod+Alt+K) · Detach Component (Mod+Alt+K) · Go to component · Detach Instance (Mod+Alt+X) · Make slot · Remove slot
6. Flip horizontal (Shift+H) · Flip vertical (Shift+V)
7. **Delete** (Backspace/Delete, destrutivo)
8. **Settings ▸**: Show pixel grid · Snap to pixel grid · Snap to objects (toggles com switch de 24×14, knob de 10 px, ligado em `#77CC66`) · separador · Open settings…

Padrões de config: `snapToObjects`, `roundToPixels` (snap to pixel grid) e `showPixelGrid` começam ligados; `smallNudge 1`, `bigNudge 10`. [C]

### 11.2 Dropdown genérico [C]

`bg-popover`, `border`, `rounded-md`, `min-w-[8rem]`, `p-1`, `shadow-md`; item `px-2 py-1.5 text-sm rounded-sm`, foco `bg-accent`.

### 11.3 Tooltip [C]

Fundo `primary` (claro `#18181b`, escuro `#fafafa`), texto `primary-foreground`, `rounded-md`, `px-3 py-1.5`, `text-xs`, `shadow-md`. O atalho vem à direita (`gap-2.5`) a 50% de opacidade. Seta de 8 px girada 45° (raio 2). `sideOffset 6`, atraso **500 ms**, não fica sob a tab bar (`collisionBoundary.y = 50`).

### 11.4 Menu da aplicação (Windows: popup pelo botão ☰) [C]

Detalhado em `docs/legacy-analysis/03-processo-principal-electron.md` §8. Resumo dos rótulos:

- **File**: New File (Ctrl+Shift+N), Open… (Ctrl+O), Open Recent ▸, Home (Ctrl+N), Import assets, Export selection ▸ (PDF, SVG, PNG, JPEG, WebP), Export snapshot, Figma import guide, Install CLI, Save (Ctrl+S), Save As… (Ctrl+Shift+S), Settings… (Ctrl+,), Close Tab (Ctrl+W).
- **Edit**: Undo (Ctrl+Z), Redo (Ctrl+Shift+Z), Cut, Copy, Paste, Delete, Select All.
- **View**: Show/Hide UI (Ctrl+\\), Agent text size ▸ (Small / Medium / Large), Toggle Full Screen.
- **Window**: Minimize, Zoom, Toggle theme, Show Previous/Next Tab (Ctrl+PageUp/PageDown), Move Tab Left/Right (Ctrl+Shift+PageUp/PageDown), Show Tab 1..8 / Show Last Tab (Ctrl+1..9).
- **Help**: Docs, Prompt gallery, pen.dev Website, extensões de IDE, Join Our Discord, Check for updates, About.

---

## 12. Painel de chat (agente)

- **Posição**: o chat é uma **aba da sidebar esquerda** ("Chat", com "Chat history" ao lado), então tem a mesma largura da sidebar (378 padrão, 280–700). Não há painel de chat à direita. [C]
- **Tela cheia**: Mod+Shift+\\. A sidebar cobre a janela inteira, com o botão "Back to canvas" (`text-[12px] font-medium`, ícone `arrow-left` de 16 px, altura de 42 px), em transição de 600 ms. [C]
- **Prompt flutuante ("spotlight")**: composer centralizado embaixo do canvas (`bottom 16`), largura padrão 625 (360–700, ajustável e salva em `pencil-spotlight-width`), `rounded-3xl` (24 px) em uma linha e `rounded-island` quando multilinha. Tem uma "notch" (aba) superior com o título da conversa e os botões "Open chat tab", "New Chat" (Mod+T) e minimizar. Placeholder: "Create, plan or research... @ to attach, / for skills" (versão curta: "Create, plan or research..."). Minimizado, vira um botão redondo de 36 px (`w-9 h-9`, `bg-island`, `shadow-xl`). [C]
- **Atividade de agentes**: chips de `h-7`, `rounded-full`, `bg-card` com borda, no canto inferior esquerdo do canvas (até 6, `max-w 28rem`). [C]
- **Texto do chat**: escalável (`--agent-text-scale`; View ▸ Agent text size, padrão "small"). [C]

---

## 13. Estados interativos (resumo)

| Estado | Regra | Fonte |
|---|---|---|
| Hover em botão ghost (toolbar, tab bar, ícones) | `bg-foreground/5` (dark `/10`) | [C] |
| Hover em item de menu de contexto | `bg-accent-active` `#3b82f6` e texto branco | [C] |
| Hover em item de select/dropdown | `bg-accent` (claro `#f4f4f5`, escuro `#3f3f46`) | [C] |
| Hover em linha dos Layers | `foreground/6` | [C] |
| Selecionado nos Layers | `blue-500/18` e texto `blue-700`/`blue-200` | [C] |
| Ferramenta ativa | variante `outline` (borda + `bg-background`) | [C] |
| Aba da sidebar selecionada | `black/5` (dark `white/10`) | [C] |
| Aba de documento ativa | `foreground/5` (dark `/10`) e texto `foreground` | [C] |
| Botão de ícone ativo (inspector) | `accent-active/20`, ícone `accent-active`; hover `/30`, pressionado `/40` | [C] |
| Segmentado ativo | `bg-white` + sombra de .5 px (dark `foreground/16%`) | [C] |
| Foco de campo do inspector | outline 1 px `#3d99ff`, offset 0 | [C] |
| Foco de botão genérico | `ring-2 ring-ring` (tab bar) ou `ring-[3px] ring-ring/50` (botões shadcn) | [C] |
| Desabilitado | opacidade 50% e sem eventos de ponteiro | [C] |
| Nó oculto | 40% (Layers e linhas de fill) | [C] |
| Tab bar coberta por diálogo | `brightness(40%)` + view inerte | [C] |
| Seleção de texto (`::selection`) | `accent-active`; no escuro, `accent-active` a 40% | [C] |
| Canvas: seleção / hover | contorno `#3d99ff`; instância `#9580ff`; componente `#d480ff`; script `#00c48c` | [C] |

---

## 14. Lacunas e incertezas

- **Lock de camada**: nenhum toggle de lock nos Layers e nenhuma string `lock` no painel. Ao que parece, o legado não tem lock por camada. Não implementar sem confirmar no schema.
- **Ícone `square` para retângulo/path/etc.**: o nome minificado `og` aparece em escopos diferentes. O mapeamento para lucide `square` saiu de `og = mt(<def "square">)`, mas pode haver sombreamento de nome [I].
- **Paths dos pen-icons**: descritos só em palavras, de propósito (não copiar). Desenhe ícones equivalentes.
- **Largura inicial da sidebar**: o config padrão é 378 e o reset do handle é 320. Ambos estão confirmados; o valor da primeira abertura depende de `sidebarWidth` persistido.
- **Área do canvas com o inspector em coluna**: o inspector no modo normal ocupa espaço no fluxo e chama `setHostInset(Right, …)` só no modo flutuante. Assumi que o zoom fica a 8 px da borda esquerda do inspector [I].
- **Espaço entre a legenda do frame e o topo do frame**: há `padding-bottom` igual a uma constante (`kie`, aparentemente 9 px), mas o binding minificado é ambíguo [I].
- **Cores compostas** (§2.4): calculadas em sRGB sobre o fundo nominal da ilha. O legado usa `color-mix(in oklab, …, transparent)`, que equivale a alfa sobre o fundo real [I].
- **Barra de título no Windows**: o legado usa frame nativo do SO. Não há botões de janela desenhados na tab bar. Se o OpenPen adotar titlebar client-side, o layout dos botões de min/max/fechar é decisão nova (não há referência).
- **Home/Dashboard, Settings, Command palette, diálogo de atalhos, Variables panel, Browser panel e Comments**: fora deste documento. Podem ser especificados depois com o mesmo método.
- **Inspector**: as seções Code, Browser, Script, Theme, Context e Component foram só listadas, sem medir cada controle.
- **Atalho do Frame**: o array da toolbar declara "F"; a doc 02 também cita "A" pelo registro de ações. Implementar os dois.

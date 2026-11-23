# pen-doc + pen-layout: cena, layout e DisplayList

## Fluxo (app)

```rust
let (mut scene, report) = pen_doc::SceneDoc::load(&bytes, Some(dir), &pen_doc::FsLibraries::default())?;
pen_layout::prewarm_system_fonts();       // opcional, no início do processo (thread própria)
let mut pipe = pen_layout::Pipeline::new(pen_layout::TextEngine::new(),   // barato: só Inter
                                         Box::new(pen_layout::FsAssets::deferred(Some(dir.into()))));
pipe.rebuild(&mut scene);                 // layout + DisplayList completos -> pipe.list
// (SceneDoc, Pipeline) é Send: pode ser montado no background e movido para a UI.
// Imagens decodificadas em background: enquanto pipe.has_pending_assets(),
// pipe.refresh_assets(&scene) troca os chunks prontos (true = lista nova).
// edição
let mut undo = pen_doc::UndoStack::new();
undo.begin();
undo.apply(&mut scene, DocOp::set(scene.node_ref(key).unwrap(), patch))?;
undo.commit();                            // um bloco = um passo de undo
pipe.update(&mut scene);                  // incremental: só as raízes sujas
undo.undo(&mut scene)?; pipe.update(&mut scene);
std::fs::write(path, scene.save())?;     // save sem perda, com o fim de linha do arquivo original
```

## pen-doc

- **Fonte da verdade = `pen_schema::Document`.** Toda `DocOp` muda o documento primeiro e depois atualiza a
  cena. Não há sincronização no save: `save()` só serializa o documento.
- **`SceneDoc`**: arena de `SceneNode` compactos (`NodeKey(u32)` de pen-render), com propriedades já resolvidas
  (variáveis, temas, instâncias). Cada nó guarda `Origin::Doc` (endereçável pelo id) ou
  `Origin::Instance { instance, path }` (o caminho `a/b` relativo à instância de documento).
  `find("inst/a/b")`, `id_path(key)` e `node_ref(key)` convertem entre chaves e endereços.
- **Instâncias**: cada nó é resolvido a partir de uma pilha de *camadas* (nó base, overrides da raiz do `ref`,
  patches de `descendants` de cada escopo de instância; o escopo mais externo vence). `descendants` com `type`
  substitui a subárvore; sem `type` é patch; `children` no patch (ou no `ref`) preenche o slot. Variantes
  (`ref` reusable usado como componente) e ciclos (viram `BrokenRef` + aviso) são tratados.
- **Bibliotecas**: `imports` são carregados via `LibraryResolver` (`FsLibraries`, `MapLibraries`, `NoLibraries`).
  Cada biblioteca é um namespace: ids aparecem como `alias:id`, `$alias:var` e eixos `alias:Mode`. Cada camada
  resolve `$vars` e `ref`s no documento que a escreveu.
- **Variáveis/temas**: temas efetivos internados (`ThemeId`), herdados e sobrescritos por `theme` do nó; valor
  temático = último que casa (regra do legado). `set_theme(eixo, valor)` muda o tema da raiz.
- **Ops**: `SetFields` (máscara de `NodeField` + valores; é a própria inversa: troca valores), `Insert`,
  `Delete`, `Move`, `Reorder`, `SetVariable`. Para `NodeRef::Override`, a entrada de `descendants` é criada
  sob demanda e removida quando fica vazia. Edição de propriedade de nó comum: re-resolve só o nó
  (`Dirty::PAINT` ou `LAYOUT`); o resto reconstrói a raiz de topo afetada e todas as raízes que expandem o
  componente editado (`uses`), **reaproveitando as chaves** pelo id path.
- `take_changes()` devolve `Changes { full, roots_changed, dirty: [(NodeKey, Dirty)] }`.
  `Dirty::MOVE` = só `x`/`y` do nó mudaram (talvez com `PAINT`); conta como layout
  (`needs_layout`), mas numa raiz de topo é só uma translação.
- **Save**: `SceneDoc::load` guarda o `SaveOptions` detectado (`LF`/`CRLF`, newline final, BOM) e
  `save()` o reproduz, então load → save de um documento sem mudanças é idêntico byte a byte.
  `SceneDoc::new` usa o padrão do legado (LF, sem newline final); `set_save_options` troca.
- Mudança em pen-schema: `swap_field` adicionado ao `pen_object!` (aditivo).

## pen-layout

- **Layout** (taffy, por raiz de topo): `horizontal/vertical` → flex sem wrap; `none` e group → filhos
  absolutos em `x/y`; fixo não encolhe; `fit_content` → `auto`; `fill_container` → `flex 1 1 0` (min 0) no
  eixo principal e `stretch` no cruzado; fora de flex usa o fallback. `layoutIncludeStroke` vira `border`.
  Rotação não afeta o layout (é aplicada no desenho, em torno do canto superior esquerdo).
- **Texto**: `TextEngine` (cosmic-text) mede e quebra as linhas (`TextLine`) uma vez, com a mesma resolução de
  família do pen-raster (`pen_fonts::system::LazyFonts`). `lineHeight` 0 = 1,2 × fontSize. Cache por
  (texto, estilo, largura).
- **Fontes sob demanda** (`pen_fonts::system`): o `FontSystem` de cada engine começa só com a Inter
  embutida (faces parseadas uma vez por processo e clonadas: `TextEngine::new` custa ~0,3 ms). As fontes
  instaladas são escaneadas **uma vez por processo** (em paralelo, ~20 ms; `prewarm_system_fonts` adianta
  isso numa thread) e cada família citada pelo documento é copiada para a engine na primeira vez que
  aparece (bytes do arquivo em memória, compartilhados entre engines). `serif`/`monospace` genéricos
  carregam a primeira família instalada de uma lista. Glifo faltando (`.notdef` num caractere visível):
  carrega as fontes de fallback da plataforma (Segoe UI, Emoji, Symbol, YaHei…) e, se ainda faltar,
  todas as faces do sistema (como arquivos, abertos sob demanda), e refaz o layout daquele texto.
  `TextEngine::with_font_system(fs)` não usa o sistema.
- **Por quê**: o `FontSystem::get_font_matches` do cosmic-text abre e parseia **todas** as faces do banco
  cujo peso difere do pedido (para ver se há eixo `wght` variável), uma vez por (família, peso, estilo).
  Com as ~400 faces do Windows como `Source::File`, isso custava ~60 ms por combinação: ~550 ms no
  `t-process-recorder.pen` (9 combinações), mais ~100 ms do scan sequencial. Com um banco pequeno e em
  memória, a mesma consulta custa microssegundos.
- **Send**: `AssetResolver: Send + Sync`, e o `TaffyTree` (o ponteiro marcado de `calc()` o torna `!Send`)
  só existe dentro de cada chamada de layout (`layout_all`/`update` criam um e o reaproveitam entre as
  raízes). `Pipeline` e `(SceneDoc, Pipeline)` são `Send` (asserção estática).
- **Imagens**: `FsAssets::new` decodifica na hora (CLI, testes). `FsAssets::deferred` decodifica numa
  thread: a primeira consulta (`AssetResolver::image_lookup`) responde `Pending`, o builder desenha o
  placeholder e marca a raiz, e `Pipeline::refresh_assets` refaz só esses chunks quando
  `AssetResolver::take_ready` avisa (o app faz polling a cada 30 ms enquanto `has_pending_assets`).
- **DisplayList**: um `Chunk` por nó de topo, compartilhado (`chunks: Vec<Arc<Chunk>>`;
  `DisplayList::from_chunks` monta a partir de chunks próprios). `update` só refaz os chunks das raízes
  sujas (versão incrementada) e mantém os demais (o mesmo `Arc`): clonar a lista para o canvas custa um
  incremento por chunk. Uma raiz de topo que só mudou `x`/`y` (`Dirty::MOVE`) não refaz o layout, e o
  chunk é transladado (itens copiados com a transformação deslocada) em vez de reconstruído. Opacidade de folha com uma única paint é dobrada no alfa; senão
  `PushLayer`. Stroke de frame com filhos é desenhado por cima dos filhos. Fill com blend ≠ normal vira layer.
  Background blur = layer vazia com `backdrop_blur` recortada pela forma. Sombras de texto = cópia borrada.

## Resultados

- Corpus (20 arquivos): todos constroem, fazem layout e geram DisplayList, sem NaN; 0 avisos de ref.
  Renderizações de conferência em `target/render-check/` (geradas pelo teste `corpus`).
- Bench (`cargo bench -p pen-layout`, release), `t-process-recorder.pen` (2995 nós, 14 chunks, 2655 itens):
  cena 3 ms; layout 8,8 ms (texto em cache) / 24 ms (texto frio); DisplayList 0,34 ms. Edição incremental de
  fill: **15 µs** (edição + update); mudança de largura (relayout da raiz) ≈ 0,28 ms.
- **Abrir como o app** (`open/t-process-recorder`: `TextEngine::new` novo, `FsAssets::deferred`, parse +
  migração + cena + layout + DisplayList): **23 ms**. Scan das fontes do sistema (`open/system_font_scan`,
  uma vez por processo): 19 ms. Processo frio (scan incluso): ≈ 50 ms. Antes: ~750 ms no app (layout +
  lista ≈ 590 ms por causa do `get_font_matches`, mais `FontSystem::new` ≈ 100 ms e Inter/ícones
  recarregados ≈ 50 ms). O bench antigo (26 ms) reaproveitava um `FontSystem` aquecido e não via isso.
- **Arrastar** (`edit/drag_step`: patch de `x` da maior raiz + `update` + `Arc<DisplayList>` novo):
  **15 µs** (antes 675 µs: relayout da raiz, rebuild do chunk e, no app, clone profundo da lista).
  Mover um nó aninhado ainda refaz o layout da raiz (≈ 0,3 ms).
- Guarda de orçamento: `apps/openpen/tests/open_budget.rs` abre o arquivo pelo `SceneDocument` num processo
  novo (scan incluso; < 150 ms em release, 20× em debug) e mede 50 passos de arrasto (< 0,3 ms em release).
  Medido: abrir 51 ms; passo de arrasto 31 µs em média (com o `flush` e o espelho de camadas do app).

## Lacunas conhecidas

- Ícones: contornos vetoriais das fontes embutidas (`pen-fonts`: lucide, Material Symbols
  Outlined/Rounded/Sharp com eixo `wght`, Phosphor; `feather` usa os nomes da lucide), emitidos como
  `Shape::Path` (igual em todos os backends). Família/nome desconhecidos viram uma caixa arredondada
  na cor do ícone (com aviso). Eixo `FILL` não é suportado (o legado também não usa).
- Texto: Inter (OFL) é embutida e carregada pelos `TextEngine` de pen-layout e pen-raster; o app GPUI
  precisa registrar `pen_layout::bundled_fonts()`.
- Shaders (fill/effect) ignorados; mesh gradient = média das cores; crop (`transform`) de imagem ignorado;
  `linearBurn`/`linearDodge` aproximados por multiply/screen.
- `script` e `browser` são placeholders; `connection` não é desenhada; rich text (spans) vira texto simples.
- Frame `layout: none` com `fit_content` sem fallback não abraça os filhos (o corpus não tem esse caso).
- Remover/recriar uma entrada de `descendants` via undo pode mudar a ordem das chaves no JSON.

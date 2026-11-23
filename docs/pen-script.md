# pen-script: o motor do `execute`

`crates/pen-script` executa os snippets JavaScript que agentes de IA mandam pela ferramenta MCP
`execute` (ver `docs/legacy-analysis/04-mcp-headless-agentes.md` §1.6 e §5.2). O snippet roda em
QuickJS (`rquickjs` 0.14, feature `parallel`) contra o documento aberto, por uma API global pequena,
como **uma transação = um passo de undo**, revertida inteira em qualquer erro não tratado.
Não depende de GPUI.

## API Rust

```rust
let mut eng = pen_script::ScriptEngine::new();          // Send; limites em eng.limits
let resp = eng.execute(&mut scene, &mut pipe, &mut undo, &mut pen_script::HeadlessHost,
    ExecuteRequest { agent_id, description, input: Some(src), edit_id: None, edits: vec![] });
// resp: ok, text (mensagem completa p/ o agente), images (PNG), created [(nome, id)],
//       edit_id (em falha), touched [Touch{id, write}] (flash), prints, warnings,
//       exported, error {message, line, column, call}, ops, duration
```

- `ScriptHost` (trait): `screenshot(scene, pipe, &RenderTarget) -> PNG` e
  `export(scene, pipe, &ExportRequest) -> paths`, com implementação padrão em CPU (pen-raster).
  `HeadlessHost` usa só os padrões; o app GPUI pode sobrescrever.
- Streaming: `execute_partial(scene, pipe, agent, prefixo)` avalia as instruções completas de um
  snippet que ainda está chegando; o `execute` final do mesmo agente com o texto completo continua
  no mesmo contexto e faz o commit (um passo de undo). `cancel_stream` reverte. Entre as chamadas o
  documento fica com as mudanças aplicadas e sem commit: o chamador não deve editar nesse meio-tempo.
- `complete_prefix(src)`: o divisor de instruções (tokenizer: strings, templates com `${}`,
  comentários, regex, profundidade de colchetes, cabeçalhos `if/for/while`, continuação por
  `else/catch/finally`, `.metodo()` e operadores). Fronteiras de quebra de linha ou `}` só são
  confirmadas quando o próximo token chega.
- Globais: `persisted_globals(agent)` / `set_persisted_globals` (para salvar com a sessão de chat),
  `forget_agent`.

## Desenho

- **Uma `Run` por chamada**: `Runtime` + `Context` novos (custo fixo ≈ 0,45 ms), com limite de heap,
  de pilha e *interrupt handler*. O prelúdio (`src/prelude.js`) define as globais
  (`Insert`, `Copy`, `Update`, `Replace`, `Move`, `Delete`, `SetVariables`, `GetVariables`, `Get`,
  `FindEmptySpace`, `Print`, `TakeScreenshot`, `Export`, `Generate`, `document`) como propriedades
  não graváveis, e todas chamam **uma** função do host `__host(nome, jsonArgs) -> json`. Erros do
  host viram `Error` lançado no ponto de chamada do snippet.
- **Estado da transação (`Tx`)** atrás de `Arc<Mutex<_>>` (as closures do rquickjs precisam ser
  `'static` + `Send`). O `SceneDoc` e o `Pipeline` do chamador são trocados (`mem::swap`) para
  dentro da `Tx` durante a avaliação e devolvidos depois; os *placeholders* vazios são
  reaproveitados entre chamadas. Pânicos são capturados e a chamada é revertida.
- **Ops**: cada função aplica `DocOp`s na hora (`SceneDoc::apply`) e guarda as inversas. Sucesso:
  `UndoStack::record(inversas)` = um passo. Erro: as inversas são aplicadas em ordem reversa.
  Layout/DisplayList são atualizados sob demanda (antes de `Get` com visitor, `FindEmptySpace`,
  screenshots) com `pipe.update`, então os `bounds` refletem os inserts da mesma chamada.
- **Dados de nó** passam por `pen_schema::node_from_value`; valores inválidos de propriedades
  conhecidas são descartados com aviso; propriedades desconhecidas ficam (sem efeito) com aviso;
  propriedades que não valem para o tipo geram aviso. Tipo desconhecido (ex.: `image`) é erro.
  Ids sempre gerados (`IdGenerator`, únicos no documento); `children` aninhados são aceitos.
- **Endereços**: `scene.find(path)` + `scene.node_ref(key)` decidem entre nó do documento
  (`NodeRef::Id`) e nó dentro de instância (`NodeRef::Override` → entrada de `descendants`).
  Inserir em `inst/slot` acrescenta `children` ao patch do slot; `Replace` em `inst/x` grava uma
  substituição tipada; ambos trocam o mapa `descendants` inteiro (inversa exata).
- **Instâncias no `Get`** (`src/resolve.rs`): expansão no nível do schema (JSON) que espelha as
  regras de camadas do builder do pen-doc (raiz do componente, overrides da raiz, patches e
  substituições de `descendants`, slots, variantes, ciclos). Usada para `Get("inst/x")`,
  `resolveInstances` e para resolver chaves de `descendants` dadas por **nome**.
- **Get com visitor**: o percurso roda em JS (para `ctx.parentCtx`, `skipChildren` e o retorno
  coletado); cada nó custa uma chamada `visit` ao host, que devolve as propriedades próprias do
  nó, `bounds` (layout no espaço do pai), `problems` (sai parcial/totalmente do pai) e os
  caminhos dos filhos.
- **Variáveis**: `SetVariables` monta os mapas novos (`variables`, `themes`, com registro
  automático de eixos/valores de tema) e aplica uma única `DocOp::ReplaceVariables` (um rebuild).
- **Screenshots/Export** são registrados durante o snippet e renderizados no fim, antes do commit,
  com a cena atualizada; falha de renderização/escrita reverte a chamada.
- **Erros**: a pilha do QuickJS traz `snippet.js:linha:coluna`; o primeiro quadro do snippet dá a
  posição e o texto da chamada (expressão com parênteses balanceados). Em streaming, cada pedaço é
  avaliado com quebras de linha/espaços à frente para manter linha e coluna.
- **editId**: o snippet que falhou fica guardado (até 64) com o agente; `edits` aplicam
  find/replace em ordem (match único salvo `all`, tentando variantes com `\n` escapado). Se a
  correção falhar de novo, o texto corrigido fica no mesmo `editId`; no sucesso o id é consumido.
- **Globais persistidas**: no sucesso, as propriedades de `globalThis` criadas pelo snippet
  (atribuição sem `const/let`, e também `var`) que são JSON (nada de funções) são serializadas por
  agente (limite total de 1 MiB, as que não cabem geram aviso) e reinjetadas na próxima chamada.
  `const`/`let` de topo são lexicais e não persistem. Numa falha, nada muda.

## Limites (`Limits`)

| Limite | Padrão | Como |
|---|---|---|
| Heap | 256 MiB | `Runtime::set_memory_limit` (falha vira "out of memory") |
| Pilha | 512 KiB | `set_max_stack_size` |
| CPU por pedaço avaliado | 2 s | *interrupt handler*; o tempo dentro do host é creditado |
| Total da chamada | 55 s | *interrupt handler* + checagem a cada chamada do host |
| Globais por agente | 1 MiB | JSON |

## Mudanças nos outros crates

- pen-doc: `DocOp::ReplaceVariables { variables, themes }` (troca os dois mapas; autoinversa; um
  rebuild), `UndoStack::record(inversas)` (registra ops aplicadas fora da pilha como um passo) e
  índice de caminhos incremental quando o `Insert` é no fim da lista (antes cada insert reindexava
  o documento inteiro: 85 → 17 ms no bench de corpus).
- pen-schema: `node_from_value`, `variable_from_value` e `clone_field_from` (gerado pelo
  `pen_object!`).

## Medidas (release, Windows)

- `cargo bench -p pen-script`: `execute/noop` ≈ 0,44 ms; `execute/insert_200` (50 linhas × 3
  filhos em loop + `Get`, documento vazio) ≈ 13,7 ms; `corpus/insert_200` (mesmo snippet no
  `t-process-recorder.pen`) ≈ 17 ms.
- Teste de corpus (`testdata/user`, 9 arquivos): varredura somente leitura com visitor em todos os
  nós + expansão de até 20 instâncias ≈ 100 ms no `t-process-recorder.pen` (2995 nós, ~15 µs por
  nó visitado); insert + update + copy + variáveis + `Get` ≈ 60 ms; undo devolve o save idêntico
  byte a byte em todos.

## Lacunas conhecidas

- `Generate` não existe (erro explicativo). `Export` só `png`/`jpeg`/`webp` (webp sem perdas);
  `pdf`, `svg` e HTML dão erro "not supported yet" (o host pode implementar).
- Funções da API são síncronas (sem `Promise`); jobs pendentes são executados depois de cada
  pedaço.
- `includePathGeometry` é ignorado. `resolveVariables` usa o tema de cada nó quando ele está na cena.
- Muitos `Insert`s numa mesma raiz de topo reconstroem a subárvore da raiz a cada op (custo
  O(n²) do pen-doc); ≈ 9 ms para 200 inserts. Um caminho rápido de insert no pen-doc resolveria.
- Expansão de instâncias de bibliotecas no `Get` não requalifica ids aninhados (`alias:`).
- `Move` não aceita destinos dentro de instâncias; `Copy` de nó interno de instância gera uma cópia
  "achatada" (instâncias aninhadas expandidas).
- `FindEmptySpace` ignora rotação e usa só os irmãos do nó âncora.
- Pilha: os 512 KiB contam a partir do topo capturado em cada `Context::with`, e o host roda em
  cima deles; com o build MSVC cada chamada JS custa ~3 KiB de pilha nativa, então a recursão JS
  para em ~150 níveis (`RangeError`, sem derrubar o processo; testado numa thread de 1 MiB, o
  tamanho da principal no Windows). Aumentar o limite exige rodar o motor numa thread com mais pilha.
- Streaming: o divisor é heurístico (não é um parser); um falso positivo só faz o pedaço falhar e
  o erro aparece no `execute` final.

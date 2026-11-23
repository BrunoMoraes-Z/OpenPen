# The `execute` tool

`execute` runs one JavaScript snippet (QuickJS, modern ES) against the open document.

- **One call = one transaction = one undo step.** If the snippet throws anywhere (and the error
  is not caught), every change of the call is reverted and nothing is applied.
- **Errors point at your code**: the response gives `line, column`, the text of the failing call
  and an `editId`. Fix the snippet by calling `execute` again with that `editId` and
  `edits: [{find, replace, all?}]` (applied in order; `find` must match exactly once unless
  `all: true`). Do not resend the whole snippet. If the patched snippet fails again, keep patching
  the same `editId`; `find` then matches the already patched text.
- **Warnings** (invalid values, unknown properties, properties that do not apply to a node type)
  are listed in the response. Invalid values are ignored; fix them in the next call.
- **Limits**: about 2 s of pure JavaScript CPU per call (time spent inside API functions does not
  count), 256 MiB of heap, a limited native stack (very deep recursion fails with a
  `RangeError`: prefer loops) and 55 s in total. Infinite loops are interrupted.
- Write plain, compact code: loops, helper functions, object spreads and template strings are
  welcome; comments are not needed.

## Ids, names and persistence

- The app always generates ids. Never set `id` on new nodes (it is ignored).
- Give every node a human readable `name`. The response maps names to the ids that were created,
  so you can use them in later calls.
- Each call has its own scope: `const`/`let`/`function` declarations are gone after the call.
  To keep a value for the next call, assign it **without** `const`/`let`:
  `heroId = Insert(...)`. Only JSON values persist (strings, numbers, booleans, arrays, plain
  objects); keep them small (ids, coordinates), never whole `Get` results. Globals of a failed call
  are discarded.
- Ids returned by `Insert`/`Copy`/`Replace` are plain strings and compose into paths:
  `cardId + "/" + labelId`.

## Addressing nodes (`path`)

A `path` is a node id. To reach a node **inside a component instance**, join ids with `/`:
`"instanceId/childId"`, and deeper for nested instances (`"page/card/title"`). Slashes only cross
instance boundaries; ordinary nesting never needs them.

`document` is the root (the strings `"root"`, `"#document"` and `"#root"` also work as parents).
Always pass id strings, never node objects: when you hold a node from `Get`, pass `node.id`.

## API

```ts
const document: string;

// Writing
function Insert(parent: string, nodeData: Node): string;
function Copy(path: string, parent: string, overrides?: Partial<Node> & { descendants?: Overrides }): string;
function Update(path: string, data: Partial<Node>): void;
function Replace(path: string, nodeData: Node): string;
function Move(path: string, parent?: string, index?: number): void;
function Delete(path: string): void;
function SetVariables(vars: Record<string, { type: "color" | "number" | "string" | "boolean"; value: Value | ThemedValue[] }>, replace?: boolean): void;

// Reading
function Get(path: string, options?: GetOptions): Node;
function Get<T>(path: string, visit: (node: Node, ctx: Ctx) => T | undefined, options?: GetOptions): T[];
function Get<T>(visit: (node: Node, ctx: Ctx) => T | undefined, options?: GetOptions): T[];
function GetVariables(): { variables: Record<string, Variable>; themes: Record<string, string[]> };
function FindEmptySpace(input: { width: number; height: number; direction?: "right" | "bottom" | "left" | "top"; padding?: number; nodeId?: string }): { x: number; y: number; parentId?: string };

// Output
function Print(...values: unknown[]): void;
function TakeScreenshot(nodeIds: string[]): void;
function Export(nodeIds: string[], format: "png" | "jpeg" | "webp" | "svg" | "pdf" | "html-css" | "html-tailwind", outputPath: string, options?: { scale?: number; quality?: number }): void;

interface GetOptions {
  depth?: number;             // without visitor: levels of children to include (0 = node only;
                              // cut-off children show as "..."). With a visitor: max depth visited.
  resolveVariables?: boolean; // "$name" values replaced by their current values
  resolveInstances?: boolean; // instances expanded into their full subtree (ids are full paths)
}

interface Ctx {
  node: Node;            // same as the first argument
  parentCtx?: Ctx;       // undefined for the starting node(s)
  depth: number;         // 0 for the starting node(s)
  index: number;         // position among its siblings
  bounds: { x: number; y: number; width: number; height: number }; // laid out, in the parent's space
  problems?: "partially clipped" | "fully clipped";                 // sticks out of its parent
  skipChildren(): void;  // do not visit this node's children
}
```

### Insert

Adds a node at the end of `parent`'s children and returns its id. `nodeData` may contain nested
`children` (each with its own `type`), so a whole card can be one `Insert`. Instances are inserted
as `{type: "ref", ref: componentId, ...rootOverrides, descendants: {...}}`. To put content into a
slot of an instance, insert into the slot's path: `Insert(instId + "/slotId", {...})`.

`descendants` keys may be the component's node ids or, when unique inside the component, their
**names** (`descendants: {Title: {content: "Billing"}}`); names are stored as ids. Paths used by
`Get`/`Update`/`Replace` (`instId + "/" + childId`) always need ids. To collect a component's ids
by name in one line: `const ids = Object.fromEntries(Get(compId, n => [n.name, n.id]))`.

### Copy

Duplicates a node (with fresh ids for the whole subtree) under `parent` and returns the new id.
Properties in `overrides` replace the copy's own properties (`name`, `x`, `y`, ...). To change
nodes *inside* the copy, use `descendants` in the same call, keyed by the **original** ids (or
paths, or unique names): the copy's descendants get new ids, so a later `Update` with the old ids
would fail. Copying a component (`reusable: true`) creates an instance of it.

### Update

Changes properties of an existing node; only the given keys change, `null` removes a property.
It cannot change `id`, `type`, `ref` or `children` (use `Replace`, `Insert`, `Move`, `Delete`).
On a node inside an instance (`"inst/child"`) it records an override. On an instance itself,
`descendants: {key: patch}` merges overrides (`null` removes one).

### Replace

Swaps a node for a new one (all properties, including position) and returns the new id. Inside an
instance (`"inst/child"`) the replacement becomes an override of that instance and the returned
path stays `"inst/child"`.

### Move

Moves a document node to `parent` (omitted = stay under the same parent) at `index` (omitted =
last). Nodes inside instances cannot be moved.

### Delete

Removes a node and its subtree. Nodes inside instances cannot be deleted: hide them with
`Update("inst/child", {enabled: false})`.

### SetVariables / GetVariables

`SetVariables` merges definitions into the document (`replace: true` replaces all of them). Each
entry is `{type, value}`; names never start with `$` (that prefix is only for references, as in
`fill: "$accent"`). Themed values are arrays of `{value, theme: {axis: value}}`; unknown theme
axes and values are registered automatically, and the first value of an axis is its default.
`GetVariables()` returns the current variables and theme axes, including changes made earlier in
the same call. Read them before defining new ones so you do not overwrite existing tokens.

```js
SetVariables({
  surface: {type: "color", value: [{value: "#FFFFFF", theme: {mode: "light"}}, {value: "#121212", theme: {mode: "dark"}}]},
  ink: {type: "color", value: [{value: "#1B1B1B", theme: {mode: "light"}}, {value: "#EDEDED", theme: {mode: "dark"}}]},
  space: {type: "number", value: 16},
  "font-body": {type: "string", value: "Inter"}
})
```

### Get

Reads the document, including changes made earlier in the same call.

- `Get(path, {depth})` returns one node as plain schema data that can be fed back to
  `Insert`/`Copy`/`Update`/`Replace`.
- With a visitor, every node of the subtree (or of the whole document when no path is given) is
  visited top-down; whatever the visitor returns, except `undefined`, is collected into the
  returned array. The visitor receives the node's own properties (children appear as `"..."`)
  and a `ctx` with the **laid out** bounds.
- Reading the whole document without a visitor is not allowed (it would be huge).
- Prefer `Print`ing one short line per node over dumping JSON.

```js
Get(screenId, (n, c) => Print(n.id, n.type, n.name, c.bounds.width + "x" + c.bounds.height))
Get(n => n.reusable && Print(n.id, n.name))
Get(screenId, (n, c) => c.problems && Print(n.name, c.problems, "in", c.parentCtx?.node.name))
Get(screenId, n => n.type === "text" && n.fontSize < 12 && Update(n.id, {fontSize: 12}))
const rows = Get(tableId, {depth: 1}).children
```

### FindEmptySpace

Finds a free `width x height` spot. Without `nodeId` it searches next to everything at the root;
with `nodeId` it searches next to that node, among its siblings (and returns `parentId` when that
node is not at the root). `direction` defaults to `"right"`, `padding` to 0. Use it before placing
anything at the root, and chain screens with `nodeId: previousScreenId`.

### Print

Adds one line to the response: strings as they are, other values as JSON, separated by spaces.
Lines printed by a failed call are not returned.

### TakeScreenshot

Renders each listed node (or `"document"` for everything) and attaches the PNG images to the
response. They show the final state of the call. Screenshots are expensive: take them when a
section is finished, of the smallest node that shows it, and use `Get` bounds for structural
checks.

### Export

Writes files: `outputPath` is a directory (relative paths are relative to the `.pen` file);
each node becomes `<nodeId>.<ext>` (`"document"` becomes `document.<ext>`). `png`, `jpeg` and
`webp` are bitmaps whose `scale` (pixels per design unit) defaults to 2; `quality` (1-100)
applies to JPEG. `svg` and `pdf` are vector files with real, selectable text (fonts embedded in
the PDF); their `scale` defaults to 1. The written paths are listed in the response.

`html-css` and `html-tailwind` export code instead: all the listed nodes go into **one** page.
`outputPath` is a directory that receives `index.html` (or a path ending in `.html` for that
file), plus `styles.css` for `html-css` and an `assets/` folder with the image fills. Flex
frames become CSS flexbox (`fit_content` hugs, `fill_container` becomes `flex: 1` / `align-self:
stretch`), other children are absolutely positioned, variables become CSS custom properties
(`var(--name)`, theme values under `[data-<axis>="<value>"]`), icons and vector shapes become
inline SVG and component instances are repeated markup preceded by a `<!-- component: Name -->`
comment. `html-tailwind` uses Tailwind v4 utilities (arbitrary values where needed) and loads
Tailwind from its CDN. Name layers well: the CSS class names come from the layer names.

### Generate

Not available in this version; calling it fails. Use image files next to the document as image
fills, or draw `path` nodes.

## Examples

A reusable component built in one call:

```js
const p = FindEmptySpace({width: 240, height: 80, padding: 80})
statId = Insert(document, {type: "frame", name: "Stat", reusable: true, x: p.x, y: p.y,
  layout: "vertical", gap: 4, padding: 16, cornerRadius: 12, fill: "#FFFFFF", stroke: "#E4E4E7", strokeWidth: 1,
  children: [
    {type: "text", name: "Label", content: "Label", fontFamily: "Inter", fontSize: 13, fill: "#71717A"},
    {type: "text", name: "Value", content: "0", fontFamily: "Inter", fontSize: 28, fontWeight: "600", fill: "#18181B"}
  ]})
const kids = Get(statId, {depth: 1}).children
statLabel = kids[0].id
statValue = kids[1].id
```

A screen with a row of instances (in a later call, using the persisted globals):

```js
const p = FindEmptySpace({width: 1280, height: 800, padding: 80})
pageId = Insert(document, {type: "frame", name: "Dashboard", x: p.x, y: p.y, width: 1280, height: 800,
  layout: "vertical", gap: 24, padding: 32, fill: "#FAFAFA"})
const row = Insert(pageId, {type: "frame", name: "Stats", layout: "horizontal", gap: 16, width: "fill_container"})
for (const [label, value] of [["Orders", "1,284"], ["Revenue", "$48.2K"], ["Customers", "9,431"]]) {
  Insert(row, {type: "ref", ref: statId, name: label, width: "fill_container",
    descendants: {[statLabel]: {content: label}, [statValue]: {content: value}}})
}
TakeScreenshot([pageId])
```

Copy a screen and adjust it in the same call:

```js
const p = FindEmptySpace({width: 1280, height: 800, padding: 80, nodeId: pageId})
v2 = Copy(pageId, document, {name: "Dashboard (dark)", x: p.x, y: p.y, theme: {mode: "dark"},
  descendants: {"Stats": {gap: 24}}})
```

Override and hide parts of an instance:

```js
Update(cardInst + "/" + titleId, {content: "Billing"})
Update(cardInst + "/" + badgeId, {enabled: false})
Replace(cardInst + "/" + iconId, {type: "icon", name: "Icon", library: "lucide", icon: "credit-card", width: 20, height: 20, fill: "#18181B"})
```

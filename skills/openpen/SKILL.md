---
name: openpen
description: Read and edit OpenPen (.pen) design documents through the `execute` tool.
---

# Working on OpenPen documents

OpenPen is a native design editor for `.pen` files. You change a document by sending small
JavaScript snippets to the `execute` tool. Before your first edit, read:

| File | When |
|---|---|
| [execute.md](./execute.md) | Always before calling `execute`: the API, its rules and examples. |
| [pen-schema.md](./pen-schema.md) | Always before creating or changing nodes: node types and properties. |

## How to work

1. **Look before you write.** Read what is there with cheap reads (`Get` with a visitor that
   prints one compact line per node, `GetVariables()`). The document may have been edited by the
   user or another agent since your last call, so re-read before relying on old ids.
2. **Do what was asked, nothing more.** Do not restyle, rename or reorganize unrelated parts.
3. **Reuse before you create.** Copy an existing screen and change it, use existing components
   (`reusable: true`) as instances (`type: "ref"`), and reference existing variables (`"$name"`).
   Never overwrite variables you did not create unless asked.
4. **Build in sections.** One `execute` per meaningful chunk (a header, a card list, a form).
   End a finished section with a `TakeScreenshot` of that section, and fix what looks wrong.
5. **Keep the root tidy.** Top-level nodes are screens, components and notes. Place new screens
   with `FindEmptySpace` so they never overlap existing ones.
6. **Fix warnings.** Every response lists invalid or unknown properties; correct them in the next
   call. When a call fails, patch it with `editId` + `edits` instead of resending it.

## Design quality checklist

- Layout: use flex frames (`layout: "vertical" | "horizontal"`, `gap`, `padding`) instead of
  absolute coordinates; children grow with `width: "fill_container"`. Check `ctx.bounds` and
  `ctx.problems` (clipped nodes) with a `Get` visitor after building.
- Text: every text node needs `fill` (its color), `fontFamily` and `fontSize`; set
  `textGrowth: "fixed-width"` with a width when it must wrap.
- Contrast and alignment: verify with a screenshot of the section, not of the whole document.
- Restraint: avoid decoration that carries no meaning (gratuitous gradients, shadows, cards inside
  cards). Never hand-draw brand logos.
- Repeated UI (rows, cards, nav items) comes from a component or a JavaScript loop, not from
  copy-pasted literals.

## Starting from a live web page

`import_website {url, width?, height?, fullPage?, selector?}` loads a page in a private
headless browser and adds it as **one new top-level frame** placed in free space, as a single
undo step. Reach for it when the user wants to recreate, restyle or redesign an existing site,
or names a page as the reference to start from. Do not use it for pages that need a login.

- It is slow (10 to 90 s): call it once per page and viewport. Use `width: 390` for a phone
  layout, `fullPage: false` for the first screen only, and `selector` (e.g. `"main"`) to take
  one part of a long page.
- The document must be saved: images and fonts land in files next to the `.pen`.
- The result is a faithful but flat copy: absolutely positioned frames, text and image layers
  named after their HTML tags. Expect to refine it (rename layers, group sections, convert them
  to flex layout, swap in variables and components) rather than to keep it as is.
- Afterwards, inspect it with `Get` on the returned frame id (with a `depth` limit or a compact
  visitor; imported pages are large) and `TakeScreenshot` of the frame. Compare with the original
  screenshot the tool returns, then edit.

## Headless sessions

When `get_app_state` says "Headless mode", no editor window is open and nobody sees the canvas:
check your work with `TakeScreenshot`, and call `save` when the task is done (nothing is written
to disk before that). There is no selection; find nodes with `Get`.

## Limits of this version

- `Generate` (AI images, stock photos, generated SVG) is not available. Put image files next to
  the `.pen` file and use them as image fills (`{type: "image", url: "./img/hero.png"}`), or draw
  vector artwork with `path` nodes.
- `Export` writes `png`, `jpeg`, `webp`, `svg` and `pdf` (no HTML/code export yet).

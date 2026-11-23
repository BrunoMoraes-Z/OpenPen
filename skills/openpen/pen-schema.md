# The `.pen` schema (version 2.22), in short

A `.pen` file is JSON: `{version, children, variables?, themes?, imports?, fonts?}`. `children` holds
the top-level nodes (screens, components, notes). Every node is an object with a `type`, an `id`
(generated for you) and type-specific properties. Absent properties take their defaults.

## Value conventions

| Kind | Form | Notes |
|---|---|---|
| Variable reference | `"$name"` | Allowed wherever a number, color, string or boolean is expected (also inside padding, corner radii, gradient stops). **Any** string starting with `$` is read as a reference, so literal text such as a price must be escaped: `"\$18,420"` in the file, `"\\$18,420"` in JS. |
| Color | `"#RGB"`, `"#RRGGBB"`, `"#RRGGBBAA"` | Transparency only through the alpha digits. |
| Size | number, `"fit_content"`, `"fill_container"`, or with a fallback: `"fit_content(320)"` | See Layout. No percentages. |
| Position | `x`, `y` numbers | Relative to the parent's top-left; ignored for children of a flex frame unless `layoutPosition: "absolute"`. |
| Angles | degrees | `rotation` turns counter-clockwise around the top-left corner. |

## Properties shared by every node

`name`, `context` (a description for agents), `enabled` (false hides it), `opacity` (0..1),
`rotation`, `flipX`, `flipY`, `x`, `y`, `layoutPosition` (`"auto"` | `"absolute"`), `theme`
(`{axis: value}` for the subtree), `reusable` (true = component), `metadata` (free object).

Most visible nodes also take `width`, `height`, `fill`, `stroke`, `strokeWidth`,
`strokeAlignment` (`"inner"` | `"center"` | `"outer"`), `strokeLinejoin`, `strokeLinecap` and
`effect`.

## Node types

| `type` | Purpose | Specific properties |
|---|---|---|
| `frame` | Container, usually with flex layout | layout properties, `cornerRadius`, `clip`, `slot`, `placeholder`, `children` |
| `group` | Container without layout (bounds = its children) | `children`; size only `fit_content`/`fill_container` |
| `rectangle` | Box | `cornerRadius` |
| `ellipse` | Ellipse, arc or ring in its box | `innerRadius` (0..1), `startAngle`, `sweepAngle` (-360..360) |
| `polygon` | Regular polygon | `polygonCount`, `cornerRadius` |
| `path` | SVG path scaled into its box | `geometry` (SVG `d`), `viewBox` `[x, y, w, h]`, `fillRule` (`"nonzero"` / `"evenodd"`) |
| `text` | Text (color comes from `fill`) | `content`, `textGrowth`, typography below |
| `icon` | Icon glyph (color from `fill`) | `library`, `icon`, `weight` (100..700) |
| `note` | Sticky note on the canvas | `content`, `color` |
| `ref` | Instance of a component | `ref`, root overrides, `descendants`, `children` (slot content) |
| `browser`, `script` | Live web page / generated content | rarely needed |

There is **no image node**: images are fills of frames or rectangles.

## Layout (frames)

| Property | Values | Default |
|---|---|---|
| `layout` | `"horizontal"`, `"vertical"`, `"none"` (children placed by `x`/`y`) | `"horizontal"` |
| `gap` | number | 0 |
| `padding` | `v`, `[vertical, horizontal]` or `[top, right, bottom, left]` | 0 |
| `justifyContent` | `"start"`, `"center"`, `"end"`, `"space_between"`, `"space_around"` (main axis) | `"start"` |
| `alignItems` | `"start"`, `"center"`, `"end"` (cross axis; there is no stretch) | `"start"` |
| `layoutIncludeStroke` | boolean | false |

Sizing:

- number: fixed size.
- `"fit_content"`: hug the children (needs a layout on the node itself); the fallback in
  parentheses is used when there is nothing to hug.
- `"fill_container"`: take the free space of a flex parent along the main axis and stretch along
  the cross axis; outside a flex parent the fallback is used. Several `fill_container` siblings
  share the space.
- A frame without `width`/`height` hugs its content.

## Text

`content` (string; `\n` breaks lines), `fontFamily`, `fontSize`, `fontWeight` (string: `"400"`,
`"600"`, `"bold"`...), `fontStyle` (`"normal"` / `"italic"`), `letterSpacing` (px), `lineHeight`
(a multiplier of the font size; 0 = font metrics), `textAlign` (`"left"`, `"center"`, `"right"`,
`"justify"`), `textAlignVertical` (`"top"`, `"middle"`, `"bottom"`), `underline`,
`strikethrough`, `href`.

`textGrowth`: `"auto"` (default; grows to fit, never wraps, ignores width/height),
`"fixed-width"` (wraps at `width`, which may be `"fill_container"`; height grows),
`"fixed-width-height"` (both fixed; may overflow). Always give text a `fill`.

## Fills, strokes and effects

`fill` and `stroke` take one paint or an array (painted in order, last on top):

- `"#hex"` or `"$var"` (solid color shorthand), or `{type: "color", color}`
- `{type: "gradient", gradientType: "linear" | "radial" | "angular", rotation, colors: [{color, position}], center?: {x, y}, size?: {width, height}, opacity?}`
  (center/size are fractions of the box; for linear gradients `rotation` 0 points up)
- `{type: "image", url, mode?: "cover" | "contain" | "stretch", opacity?}` (`url` relative to the
  `.pen` file, or http(s))

Every paint accepts `enabled` and `blendMode` (`normal`, `multiply`, `screen`, `overlay`,
`darken`, `light`, `colorBurn`, `colorDodge`, `softLight`, `hardLight`, `difference`,
`exclusion`, `hue`, `saturation`, `color`, `luminosity`, ...).

`strokeWidth` is a number or per side: `{top, right, bottom, left}` (e.g. `{bottom: 1}` for a
divider). `cornerRadius` is a number or `[topLeft, topRight, bottomRight, bottomLeft]`.

`effect` takes one effect or an array:

- `{type: "shadow", shadowType?: "outer" | "inner", offset: {x, y}, blur, color}` (no spread)
- `{type: "blur", radius}` (blurs the node)
- `{type: "background_blur", radius}` (frosted glass behind the node)

## Components and instances

- A component is any node with `reusable: true` (keep them at the top level).
- An instance is `{type: "ref", ref: "<componentId>"}`. Properties set directly on the instance
  override the component's root (`width`, `fill`, `padding`, ...).
- `descendants` overrides nodes inside the instance, keyed by the component's node ids (or paths
  `"nestedInstanceId/childId"` for instances nested in the component; in `execute`, unique node
  names are also accepted and converted to ids):
  - an object **without** `type` patches properties: `{"labelId": {content: "Save"}}`;
  - an object **with** `type` replaces that node entirely;
  - `children` inside a patch fills a slot (a frame with `slot` in the component).
- Instance internals cannot be deleted; hide them with `enabled: false`.
- A `ref` with `reusable: true` is a variant: a component built from another component.
- Ids inside an instance are the component's ids, so the editor addresses them as
  `"instanceId/childId"`.

## Variables and themes

```json
"themes": { "mode": ["light", "dark"] },
"variables": {
  "accent": { "type": "color", "value": "#2563EB" },
  "surface": { "type": "color", "value": [
    { "value": "#FFFFFF" },
    { "value": "#0A0A0A", "theme": { "mode": "dark" } } ] },
  "radius": { "type": "number", "value": 8 }
}
```

- Types: `color`, `number`, `string`, `boolean`. A value may reference another variable (`"$accent"`).
- Theme axes list their values; the first value is the default at the root. A node's `theme`
  switches axes for its subtree.
- Themed values are resolved from the **last** entry to the first: the first entry whose theme
  axes all match wins, and an entry without `theme` always matches. Put the default first and
  the specific cases after it.
- Variables of an imported library are written `"$alias:name"`; its components `"alias:id"`.

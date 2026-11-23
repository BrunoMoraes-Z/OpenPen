//! Printing: class assignment, HTML markup, stylesheet and page scaffold.

use std::collections::HashMap;
use std::fmt::Write;

use pen_doc::NodeKey;

use crate::build::{Builder, is_generic_family};
use crate::model::{Decl, El};
use crate::{CSS_FILE, CodeFormat, CodeOutput};

/// Browser build of Tailwind v4.
pub const TAILWIND_SCRIPT: &str = "https://cdn.jsdelivr.net/npm/@tailwindcss/browser@4";

const RESET_CSS: &str = "*,
::before,
::after {
  box-sizing: border-box;
}

body {
  margin: 0;
}

p {
  margin: 0;
}

img,
svg {
  display: block;
}

/* Layers paint in document order and position their absolute children, as in the design. */
[data-pen-root],
[data-pen-root] * {
  position: relative;
}
";

/// Same base rule for Tailwind, in its `base` layer (below the utilities).
const TAILWIND_BASE: &str = "@layer base {
  [data-pen-root],
  [data-pen-root] * {
    position: relative;
  }
}
";

fn text_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            c => out.push(c),
        }
    }
    out
}

fn attr_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            c => out.push(c),
        }
    }
    out
}

fn decls_key(d: &[Decl]) -> String {
    let mut s = String::new();
    for x in d {
        let _ = write!(s, "{}:{};", x.prop, x.value);
    }
    s
}

/// html-css: one class per distinct (name, declarations); elements with the same
/// layer name and the same styles share it.
struct ClassNamer {
    by_key: HashMap<(String, String), String>,
    counts: HashMap<String, u32>,
    rules: Vec<(String, Vec<Decl>)>,
}

impl ClassNamer {
    fn assign(&mut self, el: &mut El) {
        if !el.decls.is_empty() {
            let key = (el.name.clone(), decls_key(&el.decls));
            let class = match self.by_key.get(&key) {
                Some(c) => c.clone(),
                None => {
                    let n = self.counts.entry(el.name.clone()).or_insert(0);
                    *n += 1;
                    let class = if *n == 1 {
                        el.name.clone()
                    } else {
                        format!("{}-{n}", el.name)
                    };
                    self.by_key.insert(key, class.clone());
                    self.rules.push((class.clone(), el.decls.clone()));
                    class
                }
            };
            el.class = Some(class);
        }
        for c in &mut el.children {
            self.assign(c);
        }
    }
}

fn assign_tailwind(el: &mut El) {
    if !el.decls.is_empty() {
        el.class = Some(crate::tailwind::classes(&el.decls));
    }
    for c in &mut el.children {
        assign_tailwind(c);
    }
}

const VOID: &[&str] = &["img"];

fn print(el: &El, depth: usize, out: &mut String) {
    let ind = "  ".repeat(depth);
    if let Some(c) = &el.comment {
        let _ = writeln!(out, "{ind}<!-- component: {} -->", c.replace("--", "- -"));
    }
    let _ = write!(out, "{ind}<{}", el.tag);
    if let Some(c) = &el.class {
        let _ = write!(out, " class=\"{}\"", attr_escape(c));
    }
    for (k, v) in &el.attrs {
        if v.is_empty() {
            let _ = write!(out, " {k}");
        } else {
            let _ = write!(out, " {k}=\"{}\"", attr_escape(v));
        }
    }
    out.push('>');
    if VOID.contains(&el.tag) {
        out.push('\n');
        return;
    }
    if let Some(t) = &el.text {
        // Whitespace is significant (white-space: pre / pre-wrap): no indentation inside.
        let _ = writeln!(out, "{}</{}>", text_escape(t), el.tag);
        return;
    }
    if el.raw.is_empty() && el.children.is_empty() {
        let _ = writeln!(out, "</{}>", el.tag);
        return;
    }
    out.push('\n');
    for line in &el.raw {
        let _ = writeln!(out, "{ind}  {line}");
    }
    for c in &el.children {
        print(c, depth + 1, out);
    }
    let _ = writeln!(out, "{ind}</{}>", el.tag);
}

fn css_rules(rules: &[(String, Vec<Decl>)]) -> String {
    let mut out = String::new();
    for (i, (class, decls)) in rules.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let _ = writeln!(out, ".{class} {{");
        for d in decls {
            let _ = writeln!(out, "  {}: {};", d.prop, d.value);
        }
        out.push_str("}\n");
    }
    out
}

/// One Google Fonts `<link>` per family (a family missing there only breaks its own link).
fn font_links(b: &Builder<'_>) -> Vec<String> {
    let mut out = Vec::new();
    for (family, styles) in &b.fonts {
        if family.trim().is_empty() || is_generic_family(family) {
            continue;
        }
        let italic = styles.iter().any(|s| s.0);
        let mut specs: Vec<String> = styles
            .iter()
            .map(|(it, w)| {
                if italic {
                    format!("{},{w}", u8::from(*it))
                } else {
                    w.to_string()
                }
            })
            .collect();
        specs.dedup();
        let axes = if italic { "ital,wght" } else { "wght" };
        let fam = family.trim().replace(' ', "+");
        out.push(format!(
            "<link href=\"https://fonts.googleapis.com/css2?family={}:{axes}@{}&amp;display=swap\" rel=\"stylesheet\">",
            attr_escape(&fam),
            specs.join(";")
        ));
    }
    if !out.is_empty() {
        out.insert(
            0,
            "<link rel=\"preconnect\" href=\"https://fonts.gstatic.com\" crossorigin>".into(),
        );
        out.insert(
            0,
            "<link rel=\"preconnect\" href=\"https://fonts.googleapis.com\">".into(),
        );
    }
    out
}

fn indent_block(s: &str, ind: &str, out: &mut String) {
    for line in s.lines() {
        if line.is_empty() {
            out.push('\n');
        } else {
            let _ = writeln!(out, "{ind}{line}");
        }
    }
}

pub(crate) fn emit(mut b: Builder<'_>, tree: Option<El>, _roots: &[NodeKey]) -> CodeOutput {
    let opts = b.opts;
    let mut tree = tree;
    let mut css = None;
    match opts.format {
        CodeFormat::HtmlCss => {
            let mut namer = ClassNamer {
                by_key: HashMap::new(),
                counts: HashMap::new(),
                rules: Vec::new(),
            };
            if let Some(t) = &mut tree {
                namer.assign(t);
            }
            let mut s = String::from(RESET_CSS);
            let vars = b.vars.css_blocks(b.scene.document(), "");
            if !vars.is_empty() {
                s.push('\n');
                s.push_str(&vars);
            }
            if !namer.rules.is_empty() {
                s.push('\n');
                s.push_str(&css_rules(&namer.rules));
            }
            css = Some(s);
        }
        CodeFormat::HtmlTailwind => {
            if let Some(t) = &mut tree {
                assign_tailwind(t);
            }
        }
    }
    let body_depth = if opts.scaffold { 2 } else { 0 };
    let mut markup = String::new();
    if let Some(t) = &tree {
        print(t, body_depth, &mut markup);
    }
    let mut head: Vec<String> = Vec::new();
    if opts.google_fonts {
        head.extend(font_links(&b));
    }
    let tw_style = if opts.format == CodeFormat::HtmlTailwind {
        let vars = b.vars.css_blocks(b.scene.document(), "");
        if vars.is_empty() {
            TAILWIND_BASE.to_string()
        } else {
            format!("{TAILWIND_BASE}\n{vars}")
        }
    } else {
        String::new()
    };
    let html = if opts.scaffold {
        let mut h = String::from("<!doctype html>\n<html lang=\"en\">\n  <head>\n");
        h.push_str("    <meta charset=\"UTF-8\">\n");
        h.push_str(
            "    <meta name=\"viewport\" content=\"width=device-width, initial-scale=1.0\">\n",
        );
        let title = b.title.clone().unwrap_or_else(|| "OpenPen export".into());
        let _ = writeln!(h, "    <title>{}</title>", text_escape(&title));
        for l in &head {
            let _ = writeln!(h, "    {l}");
        }
        match opts.format {
            CodeFormat::HtmlCss => {
                let _ = writeln!(h, "    <link rel=\"stylesheet\" href=\"{CSS_FILE}\">");
            }
            CodeFormat::HtmlTailwind => {
                let _ = writeln!(h, "    <script src=\"{TAILWIND_SCRIPT}\"></script>");
                h.push_str("    <style>\n");
                indent_block(&tw_style, "      ", &mut h);
                h.push_str("    </style>\n");
            }
        }
        h.push_str("  </head>\n  <body>\n");
        h.push_str(&markup);
        h.push_str("  </body>\n</html>\n");
        h
    } else {
        let mut h = String::new();
        if !tw_style.is_empty() {
            h.push_str("<style>\n");
            h.push_str(&tw_style);
            h.push_str("</style>\n");
        }
        h.push_str(&markup);
        h
    };
    CodeOutput {
        html,
        css,
        assets: std::mem::take(&mut b.assets),
        warnings: std::mem::take(&mut b.warnings).into_iter().collect(),
    }
}

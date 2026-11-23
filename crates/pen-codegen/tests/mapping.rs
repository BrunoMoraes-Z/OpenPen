//! One test per mapping rule, on tiny inline documents.

use pen_codegen::{CodeFormat, CodeOutput, CodegenOptions};
use pen_doc::{NoLibraries, SceneDoc};
use pen_layout::{NoAssets, Pipeline, TextEngine};

fn gen_with(children: &str, extra: &str, opts: CodegenOptions) -> CodeOutput {
    let json = format!(r#"{{"version":"2.22"{extra},"children":[{children}]}}"#);
    let (mut scene, _) = SceneDoc::load(json.as_bytes(), None, &NoLibraries).unwrap();
    let mut pipe = Pipeline::new(TextEngine::new(), Box::new(NoAssets));
    pipe.rebuild(&mut scene);
    pen_codegen::generate(&scene, &pipe.layout, &[], &opts)
}

fn css(children: &str) -> String {
    let out = gen_with(children, "", CodegenOptions::default());
    out.css.unwrap()
}

/// Declarations of `.class { ... }`.
fn rule(css: &str, class: &str) -> String {
    let start = css
        .find(&format!("\n.{class} {{\n"))
        .unwrap_or_else(|| panic!("no .{class} in\n{css}"));
    let body = &css[start + class.len() + 5..];
    body[..body.find('}').unwrap()].to_string()
}

#[test]
fn flex_container_and_children() {
    let c = css(
        r##"{"type":"frame","id":"r","name":"Row","width":300,"height":40,"gap":8,"padding":[4,12],
        "justifyContent":"space_between","alignItems":"center","children":[
          {"type":"rectangle","id":"a","name":"Fixed","width":20,"height":20,"fill":"#ff0000"},
          {"type":"rectangle","id":"b","name":"Grow","width":"fill_container","height":"fill_container","fill":"#00ff00"},
          {"type":"rectangle","id":"c","name":"Pinned","layoutPosition":"absolute","x":5,"y":6,"width":10,"height":10,"fill":"#0000ff"}
        ]}"##,
    );
    let r = rule(&c, "row");
    for d in [
        "display: flex;",
        "justify-content: space-between;",
        "align-items: center;",
        "gap: 8px;",
        "padding: 4px 12px;",
        "width: 300px;",
    ] {
        assert!(r.contains(d), "{d} missing in {r}");
    }
    assert!(!r.contains("flex-direction"), "{r}");
    let fixed = rule(&c, "fixed");
    assert!(
        fixed.contains("width: 20px;") && fixed.contains("flex-shrink: 0;"),
        "{fixed}"
    );
    let grow = rule(&c, "grow");
    assert!(
        grow.contains("flex: 1 1 0;")
            && grow.contains("min-width: 0;")
            && grow.contains("align-self: stretch;"),
        "{grow}"
    );
    assert!(!grow.contains("flex-shrink"), "{grow}");
    let pinned = rule(&c, "pinned");
    assert!(
        pinned.contains("position: absolute;")
            && pinned.contains("left: 5px;")
            && pinned.contains("top: 6px;"),
        "{pinned}"
    );
    // Base rule: every element relative (absolute children resolve against their parent).
    assert!(c.contains("[data-pen-root] *"), "{c}");
}

#[test]
fn fit_content_root_and_layout_none_sizes() {
    let c = css(
        r##"{"type":"frame","id":"r","name":"Hug","layout":"vertical","children":[
          {"type":"frame","id":"g","name":"Free","layout":"none","width":"fit_content(50)","height":40,"children":[
            {"type":"rectangle","id":"x","x":0,"y":0,"width":30,"height":30,"fill":"#000"}]}
        ]}"##,
    );
    let r = rule(&c, "hug");
    assert!(
        r.contains("flex-direction: column;") && r.contains("width: fit-content;"),
        "{r}"
    );
    // A frame without layout cannot hug in CSS: its computed size is written.
    let f = rule(&c, "free");
    assert!(f.contains("width: ") && f.contains("height: 40px;"), "{f}");
}

#[test]
fn fills_strokes_effects() {
    let c = css(
        r##"{"type":"frame","id":"r","name":"Box","layout":"none","width":100,"height":50,
        "cornerRadius":[4,8,4,8],"clip":true,
        "fill":["#ff0000",{"type":"gradient","gradientType":"linear","rotation":180,
            "colors":[{"color":"#00000000","position":0},{"color":"#000000","position":1}]}],
        "stroke":"#111111","strokeWidth":2,"strokeAlignment":"center",
        "effect":[{"type":"shadow","offset":{"x":0,"y":2},"blur":4,"color":"#00000033"},
                  {"type":"shadow","shadowType":"inner","offset":{"x":1,"y":1},"blur":0,"color":"#ffffff"},
                  {"type":"background_blur","radius":20}],
        "children":[{"type":"rectangle","id":"k","name":"Kid","x":0,"y":0,"width":10,"height":10,"fill":"#fff",
          "strokeWidth":{"bottom":1},"stroke":"#ccc","strokeAlignment":"inner","rotation":90,"opacity":0.5}]}"##,
    );
    let r = rule(&c, "box");
    for d in [
        "border-radius: 4px 8px;",
        "background: linear-gradient(180deg, #00000000 0%, #000000 100%), #ff0000;",
        "outline: 2px solid #111111;",
        "outline-offset: -1px;",
        "box-shadow: inset 1px 1px 0 0 #ffffff, 0 2px 4px 0 #00000033;",
        "backdrop-filter: blur(10px);",
        "overflow: hidden;",
    ] {
        assert!(r.contains(d), "{d} missing in {r}");
    }
    let k = rule(&c, "kid");
    for d in [
        "box-shadow: inset 0 -1px 0 0 #cccccc;",
        "transform: rotate(-90deg);",
        "transform-origin: 0 0;",
        "opacity: 0.5;",
    ] {
        assert!(k.contains(d), "{d} missing in {k}");
    }
}

#[test]
fn text_styles() {
    let c = css(
        r##"{"type":"text","id":"t","name":"Label","content":"a <b> & c","fontFamily":"Roboto Mono",
        "fontSize":13,"fontWeight":"500","letterSpacing":0.2,"lineHeight":1.4,"textAlign":"right",
        "strikethrough":true,"fill":"#333333",
        "effect":{"type":"shadow","offset":{"x":0,"y":1},"blur":2,"color":"#00000080"}}"##,
    );
    let r = rule(&c, "label");
    for d in [
        "font-family: \"Roboto Mono\", monospace;",
        "font-size: 13px;",
        "font-weight: 500;",
        "line-height: 1.4;",
        "letter-spacing: 0.2px;",
        "text-align: right;",
        "color: #333333;",
        "text-decoration-line: line-through;",
        "white-space: pre;",
        "text-shadow: 0 1px 2px #00000080;",
    ] {
        assert!(r.contains(d), "{d} missing in {r}");
    }
    let out = gen_with(
        r##"{"type":"text","id":"t","name":"Label","content":"a <b> & c","fill":"#000"}"##,
        "",
        CodegenOptions::default(),
    );
    assert!(
        out.html.contains(">a &lt;b&gt; &amp; c</p>"),
        "{}",
        out.html
    );
    assert!(out.html.contains("family=Inter:wght@400"), "{}", out.html);
}

#[test]
fn vectors_are_inline_svg() {
    let out = gen_with(
        r##"{"type":"path","id":"p","name":"Tick","width":20,"height":10,"geometry":"M0 0 L 20 10",
        "stroke":"#00ff00","strokeWidth":2,"strokeLinecap":"round"},
        {"type":"polygon","id":"q","name":"Hex","x":40,"y":0,"width":20,"height":20,"polygonCount":6,"fill":"#123456"}"##,
        "",
        CodegenOptions::default(),
    );
    assert!(
        out.html
            .contains("<svg class=\"tick\" viewBox=\"0 0 20 10\""),
        "{}",
        out.html
    );
    assert!(
        out.html
            .contains("<path d=\"M0 0L20 10\" fill=\"none\" stroke=\"#00ff00\" stroke-width=\"2\" stroke-linecap=\"round\"/>"),
        "{}",
        out.html
    );
    assert!(out.html.contains("fill=\"#123456\""), "{}", out.html);
}

#[test]
fn variables_and_themes() {
    let out = gen_with(
        r##"{"type":"frame","id":"r","name":"Panel","width":10,"height":10,"fill":"$bg","cornerRadius":"$r",
           "theme":{"mode":"dark"}}"##,
        r##","themes":{"mode":["light","dark"]},"variables":{
           "bg":{"type":"color","value":[{"value":"#ffffff"},{"value":"#000000","theme":{"mode":"dark"}}]},
           "r":{"type":"number","value":6}}"##,
        CodegenOptions::default(),
    );
    let c = out.css.unwrap();
    assert!(c.contains(":root {\n  --bg: #ffffff;\n  --r: 6;\n}"), "{c}");
    assert!(
        c.contains("[data-mode=\"dark\"] {\n  --bg: #000000;\n}"),
        "{c}"
    );
    let r = rule(&c, "panel");
    assert!(r.contains("background-color: var(--bg);"), "{r}");
    assert!(r.contains("border-radius: calc(var(--r) * 1px);"), "{r}");
    assert!(out.html.contains("data-mode=\"dark\""), "{}", out.html);
}

#[test]
fn components_classes_and_options() {
    let children = r##"{"type":"frame","id":"c","name":"Chip","reusable":true,"padding":4,"fill":"#eee",
        "children":[{"type":"text","id":"l","name":"Text","content":"A","fill":"#000"}]},
        {"type":"frame","id":"w","name":"Wrap","x":0,"y":100,"gap":4,"children":[
          {"type":"ref","id":"i1","ref":"c"},
          {"type":"ref","id":"i2","ref":"c","descendants":{"l":{"content":"B"}}},
          {"type":"ref","id":"i3","ref":"c","fill":"#f00"}]}"##;
    let out = gen_with(children, "", CodegenOptions::default());
    assert_eq!(
        out.html.matches("<!-- component: Chip -->").count(),
        3,
        "{}",
        out.html
    );
    // Same name + same styles share a class; a different fill gets `chip-N`.
    let html = &out.html;
    assert!(html.matches("class=\"chip-2\"").count() == 2, "{html}");
    assert!(html.contains("class=\"chip-3\""), "{html}");

    let opts = CodegenOptions {
        scaffold: false,
        layer_names: true,
        layer_ids: true,
        component_comments: false,
        google_fonts: false,
        ..CodegenOptions::default()
    };
    let out = gen_with(children, "", opts);
    assert!(!out.html.contains("<!doctype"), "{}", out.html);
    assert!(!out.html.contains("<!--"), "{}", out.html);
    assert!(
        out.html.contains("data-name=\"Wrap\" data-id=\"w\""),
        "{}",
        out.html
    );
    assert!(out.html.contains("data-id=\"i2/l\""), "{}", out.html);
}

#[test]
fn tailwind_document() {
    let out = gen_with(
        r##"{"type":"frame","id":"r","name":"Card","layout":"vertical","width":200,"padding":16,"gap":8,
           "cornerRadius":12,"fill":"$bg","children":[
           {"type":"text","id":"t","content":"Hi","fontSize":18,"fontWeight":"700","fill":"#111827"}]}"##,
        r##","variables":{"bg":{"type":"color","value":"#fafafa"}}"##,
        CodegenOptions::with_format(CodeFormat::HtmlTailwind),
    );
    assert!(out.css.is_none());
    let h = &out.html;
    assert!(h.contains("@tailwindcss/browser@4"), "{h}");
    assert!(h.contains("--bg: #fafafa;"), "{h}");
    assert!(h.contains("@layer base"), "{h}");
    assert!(
        h.contains("class=\"flex flex-col items-start gap-[8px] p-[16px] w-[200px] h-fit rounded-[12px] bg-(color:--bg)\""),
        "{h}"
    );
    assert!(h.contains("text-[18px] font-bold"), "{h}");
}

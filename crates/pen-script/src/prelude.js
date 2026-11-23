// OpenPen `execute` runtime: the global API seen by snippets. Every call goes
// through one host function with JSON arguments; host errors become JS Errors
// thrown from the snippet's call site (the engine reports its line:column).
(function () {
  "use strict";
  const host = globalThis.__host;
  delete globalThis.__host;

  function call(name, args) {
    const r = JSON.parse(host(name, JSON.stringify(args)));
    if (r.e !== undefined) {
      throw new Error(r.e);
    }
    return r.v;
  }

  function target(v, what) {
    if (typeof v === "string") return v;
    if (v && typeof v === "object" && typeof v.id === "string") {
      throw new TypeError(
        what + " must be an id string, not a node object; pass its .id (\"" + v.id + "\")"
      );
    }
    throw new TypeError(what + " must be an id/path string, got " + (v === null ? "null" : typeof v));
  }

  function format(v) {
    if (typeof v === "string") return v;
    if (v === undefined) return "undefined";
    if (typeof v === "function") return "[function]";
    try {
      const s = JSON.stringify(v);
      return s === undefined ? String(v) : s;
    } catch (e) {
      return String(v);
    }
  }

  const api = {
    Insert(parent, nodeData) {
      return call("insert", { parent: target(parent, "Insert parent"), data: nodeData });
    },
    Copy(path, parent, copyNodeData) {
      return call("copy", {
        path: target(path, "Copy path"),
        parent: target(parent, "Copy parent"),
        data: copyNodeData,
      });
    },
    Update(path, updateData) {
      call("update", { path: target(path, "Update path"), data: updateData });
    },
    Replace(path, nodeData) {
      return call("replace", { path: target(path, "Replace path"), data: nodeData });
    },
    Move(path, parent, index) {
      call("move", {
        path: target(path, "Move path"),
        parent: parent === undefined || parent === null ? undefined : target(parent, "Move parent"),
        index: index,
      });
    },
    Delete(path) {
      call("delete", { path: target(path, "Delete path") });
    },
    SetVariables(variables, replace) {
      call("setVariables", { vars: variables, replace: replace === true });
    },
    GetVariables() {
      return call("getVariables", {});
    },
    FindEmptySpace(input) {
      return call("findEmptySpace", { input: input });
    },
    Print(...values) {
      call("print", { line: values.map(format).join(" ") });
    },
    TakeScreenshot(nodeIds) {
      call("screenshot", { ids: nodeIds });
    },
    Export(nodeIds, format, outputPath, options) {
      call("export", { ids: nodeIds, format: format, outputPath: outputPath, options: options });
    },
    Generate(type) {
      throw new Error(
        "Generate(\"" + type + "\") is not supported in OpenPen yet: create images with your own tools, " +
          "save them next to the .pen file and use them as image fills (url relative to the file), " +
          "or draw vector artwork as path nodes"
      );
    },
    Get(a, b, c) {
      let path, visit, opts;
      for (const x of [a, b, c]) {
        if (x === undefined || x === null) continue;
        if (typeof x === "string") path = x;
        else if (typeof x === "function") visit = x;
        else if (typeof x === "object") opts = x;
        else throw new TypeError("Get: unexpected argument of type " + typeof x);
      }
      opts = opts || {};
      const isRoot = path === undefined || path === "document" || path === "root" ||
        path === "#document" || path === "#root";
      if (!visit) {
        if (isRoot) {
          throw new Error(
            "Get() without a visitor cannot read the whole document; pass a node id " +
              "(Get(id, {depth: 2})) or a visitor (Get(n => ...))"
          );
        }
        return call("get", { path: path, opts: opts });
      }
      const out = [];
      const maxDepth = typeof opts.depth === "number" ? opts.depth : Infinity;
      const walk = (p, parentCtx, depth, index) => {
        const info = call("visit", { path: p, opts: opts });
        let skip = false;
        const ctx = {
          node: info.node,
          parentCtx: parentCtx,
          depth: depth,
          index: index,
          bounds: info.bounds,
          skipChildren() {
            skip = true;
          },
        };
        if (info.problems) ctx.problems = info.problems;
        const r = visit(info.node, ctx);
        if (r !== undefined) out.push(r);
        if (!skip && depth < maxDepth) {
          for (let i = 0; i < info.children.length; i++) {
            walk(info.children[i], ctx, depth + 1, i);
          }
        }
      };
      const roots = isRoot ? call("roots", {}) : [path];
      for (let i = 0; i < roots.length; i++) walk(roots[i], undefined, 0, i);
      return out;
    },
  };

  for (const k of Object.keys(api)) {
    Object.defineProperty(globalThis, k, { value: api[k], writable: false, enumerable: false });
  }
  Object.defineProperty(globalThis, "document", {
    value: "document",
    writable: false,
    enumerable: false,
  });

  const builtin = new Set(Object.getOwnPropertyNames(globalThis));
  builtin.add("__exportGlobals");
  builtin.add("__importGlobals");

  Object.defineProperty(globalThis, "__exportGlobals", {
    value: function (maxBytes) {
      const out = {};
      const skipped = [];
      let total = 0;
      for (const k of Object.getOwnPropertyNames(globalThis)) {
        if (builtin.has(k)) continue;
        let v;
        try {
          v = globalThis[k];
        } catch (e) {
          continue;
        }
        if (v === undefined || typeof v === "function" || typeof v === "symbol") continue;
        let s;
        try {
          s = JSON.stringify(v);
        } catch (e) {
          skipped.push(k);
          continue;
        }
        if (s === undefined) continue;
        if (total + s.length > maxBytes) {
          skipped.push(k);
          continue;
        }
        total += s.length;
        out[k] = s;
      }
      return JSON.stringify({ values: out, skipped: skipped });
    },
    writable: false,
    enumerable: false,
  });
  Object.defineProperty(globalThis, "__importGlobals", {
    value: function (json) {
      const values = JSON.parse(json);
      for (const k of Object.keys(values)) {
        try {
          globalThis[k] = JSON.parse(values[k]);
        } catch (e) {}
      }
    },
    writable: false,
    enumerable: false,
  });
})();

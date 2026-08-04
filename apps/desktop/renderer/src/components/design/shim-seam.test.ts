import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";

import designFixtures from "../../../../../../contract/design-fixtures.json";

import { markCompositionNodes, NODE_MARK_OPEN, rootNodeKey } from "../specs/composition-markers";
import { compositionNodes } from "../specs/composition-nodes";
import {
  buildFrameSrcdoc,
  DEPOT_FRAME_SOURCE,
  DEPOT_HOST_SOURCE,
  FRAME_GLOBAL_PREFIX,
  FRAME_GLOBALS,
  FRAME_MESSAGE_TYPES,
  HOST_MESSAGE_TYPES,
  PICK_CLASSES,
} from "./frame-srcdoc";
import { PROTO_SHIM_SOURCE } from "./proto-shim";

/**
 * THE SEAM TEST — the shim confronted with what really produces what it reads (finding
 * `01KZ3JTVM23C3MBATJZ3RYVPY6`, families 4, 5 and 6).
 *
 * # Why this file exists rather than a shared constant
 *
 * `PROTO_SHIM_SOURCE` is a **string**. It is inlined into a sandboxed `srcdoc` with a CSP nonce and
 * evaluated inside a frame that has no module system, no bundler and no access to this codebase — so
 * it can import NOTHING. Every name it shares with the app is therefore spelled twice by
 * construction: the marker prefix `composition-markers.ts` writes, the `window.__nyx*` globals
 * `frame-srcdoc.ts` writes, the message names and the two `source` names the host and the frame
 * agree on. Three of those pairs had a constant on ONE side that nobody imported and a literal
 * re-typed on the other, which is the same thing as having no constant at all.
 *
 * What replaces the impossible import is this: run the shim source over DOM and globals produced by
 * the REAL producers, and read the shim's own source for the names it really asks for. A rename on
 * either half then fails here — which is what "arrimage" means, as opposed to a copy that goes on
 * looking right while the gesture underneath it is dead.
 *
 * # What a break here looks like in the app
 *
 * Nothing throws. A marker prefix that no longer matches indexes zero nodes, so every click in the
 * page designates nothing: no params open, no comment can be pinned. A global name that no longer
 * matches reads `undefined`, so a component with three axes renders as one with none. A message name
 * that no longer matches is IGNORED by the other side, because both sides guard on it. Silence, in
 * all three cases — the exact failure mode of every divergence this branch has already produced.
 */

/** Run the exact inlined shim source against the current jsdom document. */
function installShim() {
  (0, eval)(PROTO_SHIM_SOURCE);
}

/**
 * The DISTINCT messages of one type the shim posted, as parsed objects.
 *
 * Every test in this file installs the shim into the SAME jsdom document, and a document-level
 * listener cannot be uninstalled — so one click is answered by every instance ever installed, with
 * the same payload. What is asserted is therefore the SET of answers, never how many times it was
 * given. (Same reasoning, and same helper, as `proto-shim.test.ts`.)
 */
function postedOfType(spy: ReturnType<typeof vi.spyOn>, type: string): Record<string, unknown>[] {
  const calls = spy.mock.calls as unknown as unknown[][];
  const all = calls.map((c) => c[0] as Record<string, unknown>);
  const seen = all.filter((m) => m.type === type).map((m) => JSON.stringify(m));
  return [...new Set(seen)].map((m) => JSON.parse(m) as Record<string, unknown>);
}

/** Post a message INTO the frame the way the host does. */
function fromHost(message: Record<string, unknown>) {
  window.dispatchEvent(
    new MessageEvent("message", { data: { source: DEPOT_HOST_SOURCE, ...message } }),
  );
}

/**
 * Build the frame's DOM from a composition, THE WAY THE REAL CHAIN DOES: the composition goes
 * through {@link markCompositionNodes} — the real producer of the node markers — and the resulting
 * tree is emitted the way the sidecar's lenient render emits it (an `[inline]` node's markup
 * VERBATIM, a component node as an element wrapping its children).
 *
 * Not one marker string is typed here: they come out of the module under test. That is the whole
 * point — the fixture DOM of `proto-shim.test.ts` spells `nyx-node:Root%2FCard` by hand, which is a
 * THIRD copy of the convention and cannot fail when the first two drift apart.
 */
function renderMarked(composition: unknown): string {
  const marked = markCompositionNodes(JSON.stringify(composition));
  const emit = (node: Record<string, unknown>): string => {
    const kids = Array.isArray(node.children) ? (node.children as Record<string, unknown>[]) : [];
    const inner = kids.map(emit).join("");
    if (typeof node.inline === "string") return node.inline;
    const name = typeof node.component === "string" ? node.component : "structural";
    return `<div data-component="${name}">${inner}</div>`;
  };
  return emit(JSON.parse(marked ?? "{}") as Record<string, unknown>);
}

/** Parse a markup fragment into real nodes, COMMENTS INCLUDED — the markers are comments. */
function parseFragment(markup: string): Node[] {
  const parsed = new DOMParser().parseFromString(`<body>${markup}</body>`, "text/html");
  return [...parsed.body.childNodes].map((node) => document.importNode(node, true));
}

/** `#depot-root` holding `markup`, plus the chip-bar host the shim looks for. */
function mountFrame(markup: string): HTMLElement {
  document.body.replaceChildren();
  const bar = document.createElement("div");
  bar.id = "depot-chip-bar";
  const root = document.createElement("div");
  root.id = "depot-root";
  root.append(...parseFragment(markup));
  document.body.append(bar, root);
  return root;
}

/** Every `window.__nyx*` global, cleared — a test must never inherit another's. */
function clearGlobals() {
  for (const name of FRAME_GLOBALS) {
    delete (window as unknown as Record<string, unknown>)[`${FRAME_GLOBAL_PREFIX}${name}`];
  }
}

/** Write ONE global the way the srcdoc's bootstrap does, through the shared prefix. */
function setGlobal(name: string, value: unknown) {
  (window as unknown as Record<string, unknown>)[`${FRAME_GLOBAL_PREFIX}${name}`] = value;
}

/** Click an element the way a user does — the shim delegates on the document. */
function click(element: Element) {
  element.dispatchEvent(new MouseEvent("click", { bubbles: true }));
}

let postSpy: ReturnType<typeof vi.spyOn>;

beforeEach(() => {
  clearGlobals();
  postSpy = vi.spyOn(window.parent, "postMessage").mockImplementation(() => {});
});

afterEach(() => {
  postSpy.mockRestore();
  document.body.replaceChildren();
  clearGlobals();
});

/**
 * FAMILY 4 — THE NODE MARKERS. `composition-markers.ts` exports `NODE_MARK_OPEN` / `NODE_MARK_CLOSE`
 * and nobody imported them; the shim re-typed both literals in its own source. A coherent rename
 * (the constant and every caller) left the shim on the old prefix, both suites green, and the
 * central gesture of the screen — click a component in the render, open its params — dead.
 */
describe("the seam: the node markers the host asks for are the ones the shim reads", () => {
  const COMPOSITION = {
    key: "Root",
    component: "Page",
    children: [
      {
        component: "Card",
        children: [
          { component: "Button", key: "Save", children: [{ inline: "<span>Save</span>" }] },
        ],
      },
      { component: "Aside" },
      // A HOSTILE segment label: `-` and `>` are exactly what would let a node's own name
      // terminate the comment it travels in (`-->`). The producer escapes them and the shim decodes
      // them back — a pair that has to be exercised TOGETHER, or the region silently breaks in half.
      { component: "Odd", key: "a-->b" },
    ],
  };

  test("a click resolves to the node_key the SHARED convention produced", () => {
    const root = mountFrame(renderMarked(COMPOSITION));
    // The root's key is handed over separately — `#depot-root` is the one node the markup cannot
    // name — and it comes from the real producer too.
    setGlobal("RootNode", rootNodeKey(JSON.stringify(COMPOSITION)));
    installShim();
    fromHost({ type: "pick-mode", mode: "select" });
    postSpy.mockClear();

    // The DEEPEST node wins, and the key it must answer with is READ OFF the other implementation
    // of the same convention rather than typed here: `<span>` is the `[inline]` child of the Save
    // button, so it belongs to that inline node, not to the button.
    const keys = compositionNodes(JSON.stringify(COMPOSITION)).map((n) => n.nodeKey);
    const deepest = keys.find((k) => k.endsWith("/Save/inline"));
    expect(deepest, "the convention names the inline child of the Save button").toBeTruthy();
    click(root.querySelector("span")!);
    expect(postedOfType(postSpy, "select")).toEqual([
      { source: DEPOT_FRAME_SOURCE, type: "select", nodeKey: deepest },
    ]);

    postSpy.mockClear();
    click(root.querySelector('[data-component="Aside"]')!);
    expect(postedOfType(postSpy, "select")).toEqual([
      {
        source: DEPOT_FRAME_SOURCE,
        type: "select",
        nodeKey: keys.find((k) => k.endsWith("/Aside")),
      },
    ]);

    // …and the node whose own NAME could have cut its marker in half resolves like any other.
    postSpy.mockClear();
    click(root.querySelector('[data-component="Odd"]')!);
    expect(postedOfType(postSpy, "select")).toEqual([
      {
        source: DEPOT_FRAME_SOURCE,
        type: "select",
        nodeKey: keys.find((k) => k.endsWith("/a-->b")),
      },
    ]);
  });

  test("every node of the tree is reachable — the two walks name the same set", () => {
    mountFrame(renderMarked(COMPOSITION));
    setGlobal("RootNode", rootNodeKey(JSON.stringify(COMPOSITION)));
    installShim();

    // `compositionNodes` is the OTHER reader of the same convention (the tree pane, the comment
    // anchors). Every key it names must be an element the shim can ring, or a comment pinned from
    // the tree would highlight nothing in the render.
    const keys = compositionNodes(JSON.stringify(COMPOSITION)).map((n) => n.nodeKey);
    expect(keys.length).toBeGreaterThan(3);
    for (const nodeKey of keys) {
      fromHost({ type: "highlight-node", nodeKey });
      expect(
        document.querySelectorAll(`.${PICK_CLASSES.selected}`).length,
        `${nodeKey} is a region the shim can ring`,
      ).toBeGreaterThan(0);
    }
  });

  test("the marker prefix the shim scans for is the one the module writes", () => {
    // The behavioural tests above already bite; this states the pair in one line, so the failure
    // NAMES the divergence instead of only showing a gesture that stopped working.
    expect(PROTO_SHIM_SOURCE).toContain(`var MARK_OPEN = "${NODE_MARK_OPEN}";`);
    expect(markCompositionNodes(JSON.stringify(COMPOSITION)) ?? "").toContain(
      `<!--${NODE_MARK_OPEN}`,
    );
  });
});

/**
 * FAMILY 5 — THE WINDOW NAMES AND THE TEN MESSAGE TYPES. Seven out, three in, and both sides guard
 * on the name: a message that does not name the other side is dropped without a word. There was no
 * shared constant at all, so a renamed message was a feature that silently stopped happening.
 */
describe("the seam: the message channel between the frame and the host", () => {
  test("the frame posts EXACTLY the seven declared types, under the declared source name", () => {
    const posted = [...PROTO_SHIM_SOURCE.matchAll(/post\(\{\s*type:\s*"([a-z-]+)"/g)].map(
      (m) => m[1]!,
    );
    expect([...new Set(posted)].sort()).toEqual([...FRAME_MESSAGE_TYPES].sort());
    // …and the `source` stamped on every one of them is the constant the host filters on.
    const sources = [...PROTO_SHIM_SOURCE.matchAll(/source:\s*"([a-z-]+)"/g)].map((m) => m[1]!);
    expect([...new Set(sources)]).toEqual([DEPOT_FRAME_SOURCE]);
  });

  test("the frame listens for EXACTLY the three declared host types, from the declared source", () => {
    const handled = [...PROTO_SHIM_SOURCE.matchAll(/data\.type === "([a-z-]+)"/g)].map(
      (m) => m[1]!,
    );
    expect([...new Set(handled)].sort()).toEqual([...HOST_MESSAGE_TYPES].sort());
    expect(PROTO_SHIM_SOURCE).toContain(`data.source !== "${DEPOT_HOST_SOURCE}"`);
  });

  test("each declared host type really DOES something, and a foreign source does nothing", () => {
    mountFrame(renderMarked({ key: "Root", children: [{ component: "Card" }] }));
    setGlobal("RootNode", "Root");
    installShim();

    // `highlight-node` rings a node…
    fromHost({ type: "highlight-node", nodeKey: "Root/Card" });
    expect(document.querySelectorAll(`.${PICK_CLASSES.selected}`).length).toBe(1);
    // …`pinned-nodes` marks the commented ones, but only while PINNING…
    fromHost({ type: "pinned-nodes", nodeKeys: ["Root/Card"] });
    expect(document.querySelectorAll(`.${PICK_CLASSES.pinned}`).length).toBe(0);
    fromHost({ type: "pick-mode", mode: "pin" });
    expect(document.querySelectorAll(`.${PICK_CLASSES.pinned}`).length).toBe(1);
    // …and `pick-mode` mounts the capture layer that swallows a pinning click.
    expect(document.querySelectorAll(`.${PICK_CLASSES.capture}`).length).toBeGreaterThan(0);
    fromHost({ type: "pick-mode", mode: "off" });
    expect(document.querySelectorAll(`.${PICK_CLASSES.capture}`).length).toBe(0);

    // A message that does not NAME this host is not ours, whatever it claims to be.
    window.dispatchEvent(
      new MessageEvent("message", {
        data: { source: "someone-else", type: "highlight-node", nodeKey: null },
      }),
    );
    expect(document.querySelectorAll(`.${PICK_CLASSES.selected}`).length).toBe(1);
  });
});

/**
 * FAMILY 6 — THE `window.__nyx*` GLOBALS. Written literally by the srcdoc builder, read by
 * CONCATENATION in the shim (`window["__nyx" + name]`): invisible to every refactor and to every
 * grep for the whole identifier. Renaming `__nyxAxes` leaves the shim reading a global nobody
 * writes — a component with three axes rendering as one with none, and no suite disagreeing.
 */
describe("the seam: the globals the srcdoc writes are the globals the shim reads", () => {
  test("the shim asks for exactly the declared names, under the declared prefix", () => {
    const asked = [...PROTO_SHIM_SOURCE.matchAll(/globalOf\("([A-Za-z]+)"\)/g)].map((m) => m[1]!);
    expect([...new Set(asked)].sort()).toEqual([...FRAME_GLOBALS].sort());
    expect(PROTO_SHIM_SOURCE).toContain(`window["${FRAME_GLOBAL_PREFIX}" + name]`);
  });

  test("the REAL bootstrap drives the REAL applier — axes, active combination and root node", () => {
    const srcdoc = buildFrameSrcdoc({
      markup: `<div data-nyx-axis="variant"></div>`,
      tailwindSource: "",
      shimSource: "",
      themeCss: "",
      nonce: "n0nce",
      states: [],
      axes: [
        {
          name: "variant",
          defaultValue: "primary",
          values: [
            { name: "primary", classes: "bg-primary" },
            { name: "ghost", classes: "bg-transparent" },
          ],
        },
        {
          name: "loading",
          defaultValue: "off",
          values: [{ name: "off" }, { name: "on", classes: "opacity-70" }],
        },
      ],
      axisValues: { variant: "ghost", loading: "on" },
      rootNode: "Root",
    });

    // The bootstrap the builder really emitted — not a hand-written stand-in.
    const scripts = [...srcdoc.matchAll(/<script nonce="n0nce">([\s\S]*?)<\/script>/g)].map(
      (m) => m[1]!,
    );
    const bootstrap = scripts.find((s) => s.includes(FRAME_GLOBAL_PREFIX));
    expect(bootstrap, "the srcdoc carries a bootstrap that writes the globals").toBeTruthy();

    mountFrame(
      `<div data-nyx-axis="variant" id="target"><i data-nyx-if="loading=on" id="spin">◌</i></div>`,
    );
    (0, eval)(bootstrap!);
    installShim();

    // The applier read the AXES, the ACTIVE COMBINATION and the ROOT KEY through the same names.
    expect(document.getElementById("target")!.className).toContain("bg-transparent");
    expect(document.getElementById("spin")!.hidden).toBe(false);
    expect(document.querySelectorAll("#depot-chip-bar .depot-chip").length).toBe(4);
    fromHost({ type: "highlight-node", nodeKey: "Root" });
    expect(document.getElementById("depot-root")!.classList.contains(PICK_CLASSES.selected)).toBe(
      true,
    );
  });
});

/**
 * INVARIANT — **THE TREE IS THE CONTRACT, NEVER A CSS SELECTOR** (finding
 * `01KZ3JTWB82DCG0C6JT06T4CAF`, point 3).
 *
 * It is the central argument of `composition-markers.ts` and of the shim's own header, and nothing
 * enforced it. Depot's feedback shim, which this one was ported from, anchors a comment on a
 * SELECTOR computed by walking up the tree until `querySelectorAll(…).length === 1`; nyx anchors on
 * a NODE of the named tree, because a selector breaks the moment a class changes and a design round
 * changes classes for a living. Re-introducing `computeSelector` would have been a fifty-line paste
 * that every existing test accepts.
 *
 * NEGATIVE, and in two ways: the shim's SOURCE must not build a selector, and no message it POSTS
 * may carry one.
 */
describe("the invariant: the anchor is a node of the tree, never a CSS selector", () => {
  test("the shim's source builds no selector — the depot idioms are absent", () => {
    // What is banned is what turns a DOM POSITION into a STRING. `querySelectorAll` is not on the
    // list and must not be: the shim legitimately clears its own marker classes with a class
    // lookup. `computeSelector` is banned as a DEFINITION and as a CALL rather than as a word,
    // because the shim's own source comment states, deliberately, that it is the one thing depot
    // shipped and nyx does not port — the ban must not make that sentence unwritable.
    for (const banned of [
      "function computeSelector",
      "computeSelector(",
      "nth-child",
      "nth-of-type",
      ":nth",
      "tagName",
      "nodeName",
      "localName",
      '"#" +',
      '" > "',
      '">" +',
    ]) {
      expect(
        PROTO_SHIM_SOURCE,
        `the shim must not compute a CSS selector (\`${banned}\`)`,
      ).not.toContain(banned);
    }
    // …and the shim SAYS SO, which is what a reader of the file needs: the refusal is a decision,
    // not an omission somebody may helpfully repair.
    expect(PROTO_SHIM_SOURCE).toContain("computeSelector");
  });

  test("no message the shim posts carries anything that could be a selector", () => {
    const root = mountFrame(
      renderMarked({
        key: "Root",
        children: [
          { component: "Card", children: [{ component: "Button", key: "Save" }] },
          { inline: `<a id="link" data-nyx-page="pricing">Pricing</a>` },
        ],
      }),
    );
    setGlobal("RootNode", "Root");
    installShim();
    fromHost({ type: "pick-mode", mode: "select" });
    postSpy.mockClear();

    click(root.querySelector('[data-component="Button"]')!);
    click(root.querySelector("#link")!);

    const all = (postSpy.mock.calls as unknown as unknown[][]).map(
      (c) => c[0] as Record<string, unknown>,
    );
    expect(all.length).toBeGreaterThan(0);
    // The payload keys are the declared vocabulary and nothing else — a `selector` field could not
    // even arrive unnoticed.
    const DECLARED_FIELDS = [
      "source",
      "type",
      "nodeKey",
      "page",
      "x",
      "y",
      "height",
      "values",
      "state",
    ];
    for (const message of all) {
      for (const key of Object.keys(message)) {
        expect(
          DECLARED_FIELDS,
          `an undeclared field on a ${String(message.type)} message`,
        ).toContain(key);
      }
      for (const [key, value] of Object.entries(message)) {
        if (typeof value !== "string") continue;
        expect(/[>~]|:nth-|^[#.]/.test(value), `${key} reads as a CSS selector: ${value}`).toBe(
          false,
        );
      }
    }
  });
});

/**
 * INVARIANT — **THE GESTURE NEVER LIES** (finding `01KZ3JTWB82DCG0C6JT06T4CAF`, point 5), for the
 * one gesture that crosses the language boundary: a link to a page that is GONE.
 *
 * The render NEUTRALIZES such a link — it clears the whole navigation vocabulary off the node's root
 * before writing the `data-nyx-page-missing` marker, so a `data-nyx-page` a component's template
 * baked in cannot survive beside it and send the shim to a slug frozen in that template (finding
 * `01KZ3JN69E32FYBF01NBE4FWJD`). That guarantee is only worth what the CONSUMER does with it, and
 * the consumer is this shim. So the markup Rust pins in `contract/design-fixtures.json` is run
 * THROUGH the shim here: a dead link must post nothing, a live one must post its slug.
 */
describe("the invariant: a link that resolves to nothing does not navigate", () => {
  const nav = designFixtures.navigation;

  test("the neutralized markup Rust pins posts NO navigation at all", () => {
    const root = mountFrame(nav.neutralisation_case.expected_markup);
    setGlobal("RootNode", "Root");
    installShim();
    postSpy.mockClear();

    click(root.querySelector("button")!);
    expect(postedOfType(postSpy, "navigate"), nav.neutralisation_case._comment).toEqual([]);
    // …and the markup really is the neutralized one: the marker is there, the link is not.
    const rendered = root.outerHTML;
    expect(rendered).toContain(nav.attributes.missing);
    for (const absent of nav.neutralisation_case.absent_attributes) {
      expect(rendered).not.toContain(`${absent}=`);
    }
  });

  test("a link the render DID resolve navigates, to the slug the render wrote", () => {
    const linked = nav.statuses.find((s) => s.code === "linked");
    expect(linked, "the fixture declares the only status that can navigate").toBeTruthy();
    const root = mountFrame(
      `<button class="btn" ${nav.attributes.page}="pricing" ${nav.attributes.page_id}="pg-1">Voir</button>`,
    );
    installShim();
    postSpy.mockClear();

    click(root.querySelector("button")!);
    expect(postedOfType(postSpy, "navigate"), linked!.why).toEqual([
      { source: DEPOT_FRAME_SOURCE, type: "navigate", page: "pricing" },
    ]);
  });
});

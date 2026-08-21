/**
 * An artist biography is an HTML FRAGMENT, not a paragraph of text.
 *
 * This was worth establishing rather than assuming. The official desktop
 * client hands `profile.biography.text` to a generic HTML-to-React converter
 * together with a `LinkComponent` and an `onLinkClick` handler — the same
 * treatment it gives playlist descriptions, whose own editor demonstrably
 * writes `<a href="…">label</a>` into the string. Its converter is configured
 * with a small transform map (`a`, `p`, `br`, plus a fallback that keeps the
 * children of anything it does not recognise), which is exactly the shape
 * modelled here. Rendering the string as plain text therefore prints angle
 * brackets at the reader; rendering it with `{@html}` hands an untrusted
 * remote string the run of the document. Neither is acceptable, so it is
 * parsed into a node tree and the tree is rendered as components.
 *
 * WHAT COMES OUT is a small, closed set of nodes. There is no passthrough
 * case: a tag this file does not name contributes its children and nothing
 * else, so no attribute of an unknown element can ever reach the DOM.
 *
 *   { kind: "text",     value }
 *   { kind: "break"     }
 *   { kind: "paragraph", children }
 *   { kind: "emphasis",  strong, children }
 *   { kind: "route",     route, id, children }   in-app: a spotify: URI
 *   { kind: "external",  href, children }        http(s) only
 *
 * A link that is neither of the last two — `javascript:`, `data:`, a relative
 * path, anything malformed — keeps its label and loses its href. A biography
 * is never a good enough reason to follow a scheme this app does not
 * understand.
 */

/** Tags that survive as themselves. Everything else is unwrapped. */
const BLOCK = new Set(["P", "DIV"]);
const STRONG = new Set(["B", "STRONG"]);
const EMPHASIS = new Set(["I", "EM"]);

/** The `spotify:<kind>:<id>` kinds this app has a route for. */
const ROUTES = { artist: "artist", album: "album", playlist: "playlist" };
const SPOTIFY_URI = /^spotify:(artist|album|playlist):([A-Za-z0-9]{22})$/;

function classifyHref(raw) {
  const href = String(raw ?? "").trim();
  const uri = SPOTIFY_URI.exec(href);
  if (uri) return { kind: "route", route: ROUTES[uri[1]], id: uri[2] };
  // Spotify also writes bios with the open.spotify.com form of the same link.
  const web = /^https?:\/\/open\.spotify\.com\/(artist|album|playlist)\/([A-Za-z0-9]{22})/.exec(href);
  if (web) return { kind: "route", route: ROUTES[web[1]], id: web[2] };
  if (/^https?:\/\//i.test(href)) return { kind: "external", href };
  return null;
}

function convert(node, out) {
  if (node.nodeType === 3) {
    const value = node.nodeValue;
    if (value) out.push({ kind: "text", value });
    return;
  }
  if (node.nodeType !== 1) return; // comments, CDATA, processing instructions

  const tag = node.tagName;
  if (tag === "BR") {
    out.push({ kind: "break" });
    return;
  }

  const children = [];
  for (const child of node.childNodes) convert(child, children);
  if (!children.length && !BLOCK.has(tag)) return;

  if (tag === "A") {
    const target = classifyHref(node.getAttribute("href"));
    // No target this app will follow: the label survives as ordinary text —
    // not emphasised, because a link we refused is not a thing to stress.
    if (target) out.push({ ...target, children });
    else out.push(...children);
    return;
  }
  if (BLOCK.has(tag)) {
    out.push({ kind: "paragraph", children });
    return;
  }
  if (STRONG.has(tag) || EMPHASIS.has(tag)) {
    out.push({ kind: "emphasis", strong: STRONG.has(tag), children });
    return;
  }
  // Unrecognised element: keep what it said, discard what it was.
  out.push(...children);
}

/**
 * Parses a biography into renderable nodes.
 *
 * `DOMParser` is used rather than an innerHTML sink because the document it
 * produces has no browsing context: nothing in the string can load a resource
 * or run, whatever it contains, and the result is only ever read as a tree.
 */
export function parseBiography(source) {
  const text = String(source ?? "").trim();
  if (!text) return [];
  try {
    const parsed = new DOMParser().parseFromString(text, "text/html");
    const nodes = [];
    for (const child of parsed.body.childNodes) convert(child, nodes);
    return nodes;
  } catch {
    // A parse that somehow fails still has to show the reader the words.
    return [{ kind: "text", value: text }];
  }
}

/** Plain text of a parsed biography — for length checks and `title`. */
export function biographyText(nodes) {
  return nodes
    .map((node) => {
      if (node.kind === "text") return node.value;
      if (node.kind === "break") return "\n";
      if (node.kind === "paragraph") return `${biographyText(node.children)}\n\n`;
      return node.children ? biographyText(node.children) : "";
    })
    .join("");
}

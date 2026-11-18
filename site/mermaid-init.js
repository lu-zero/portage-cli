// Each diagram carries its own `config:` header, so mermaid only needs to render the `pre.mermaid` blocks.
import("https://cdn.jsdelivr.net/npm/mermaid@11.4.1/dist/mermaid.esm.min.mjs").then(({ default: mermaid }) => {
  mermaid.initialize({ startOnLoad: false });
  mermaid.run({ querySelector: "pre.mermaid" });
});

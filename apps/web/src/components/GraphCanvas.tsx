import { useEffect, useRef } from "react";
import cytoscape, { type Core, type ElementDefinition } from "cytoscape";
import type { GraphEdge, GraphNode } from "@usie/sdk";

const kindShape: Record<string, string> = {
  file: "round-rectangle", directory: "round-tag", repository: "star", package: "hexagon",
  class: "diamond", interface: "diamond", function: "ellipse", method: "ellipse", type: "round-triangle",
  variable: "rectangle", namespace: "barrel",
};

function css(name: string): string {
  const v = getComputedStyle(document.documentElement).getPropertyValue(`--c-${name}`).trim();
  return v ? `rgb(${v.split(/\s+/).join(",")})` : "#888";
}

/**
 * Renders a subgraph with Cytoscape. Node shape encodes kind; edge line
 * style encodes confidence (solid confirmed, long-dash probable, dotted
 * inferred) in addition to colour.
 */
export function GraphCanvas({ nodes, edges, focusId, onSelect, layout = "cose" }: { nodes: GraphNode[]; edges: GraphEdge[]; focusId?: number; onSelect: (n: GraphNode) => void; layout?: "cose" | "breadthfirst" | "concentric" }) {
  const container = useRef<HTMLDivElement>(null);
  const cy = useRef<Core | null>(null);
  const select = useRef(onSelect);
  select.current = onSelect;

  useEffect(() => {
    if (!container.current) return;
    const byId = new Map(nodes.map((n) => [n.id, n]));
    const elements: ElementDefinition[] = [
      ...nodes.map((n) => ({
        data: { id: `n${n.id}`, label: n.kind === "file" || n.kind === "directory" ? (n.path ?? n.name) : n.name, kind: n.kind, focus: n.id === focusId ? 1 : 0 },
      })),
      ...edges.filter((e) => byId.has(e.src) && byId.has(e.dst)).map((e) => ({
        data: { id: `e${e.src}-${e.dst}-${e.kind}`, source: `n${e.src}`, target: `n${e.dst}`, label: e.kind, confidence: e.confidence },
      })),
    ];
    const conf = { confirmed: css("confirmed"), probable: css("probable"), inferred: css("inferred"), unknown: css("unknown") };
    const instance = cytoscape({
      container: container.current,
      elements,
      wheelSensitivity: 0.3,
      minZoom: 0.1,
      maxZoom: 3,
      style: [
        {
          selector: "node",
          style: {
            label: "data(label)", "font-size": 10, color: css("text"), "text-valign": "bottom", "text-margin-y": 4,
            "text-max-width": "160px", "text-wrap": "ellipsis", "background-color": css("accent"), width: 18, height: 18,
            "border-width": 1, "border-color": css("border"),
            shape: ((el: cytoscape.NodeSingular) => kindShape[el.data("kind") as string] ?? "ellipse") as unknown as cytoscape.Css.NodeShape,
          },
        },
        { selector: "node[kind = 'package']", style: { "background-color": css("info") } },
        { selector: "node[kind = 'file'], node[kind = 'directory'], node[kind = 'repository']", style: { "background-color": css("muted") } },
        { selector: "node[focus = 1]", style: { width: 28, height: 28, "border-width": 3, "border-color": css("warn"), "font-weight": "bold" } },
        { selector: "node:selected", style: { "border-width": 3, "border-color": css("text") } },
        {
          selector: "edge",
          style: {
            width: 1.5, "curve-style": "bezier", "target-arrow-shape": "triangle", "arrow-scale": 0.8,
            "line-color": conf.unknown, "target-arrow-color": conf.unknown,
            label: "data(label)", "font-size": 7, color: css("muted"), "text-rotation": "autorotate", "text-background-color": css("bg"), "text-background-opacity": 0.8,
          },
        },
        { selector: "edge[confidence = 'confirmed']", style: { "line-color": conf.confirmed, "target-arrow-color": conf.confirmed } },
        { selector: "edge[confidence = 'probable']", style: { "line-color": conf.probable, "target-arrow-color": conf.probable, "line-style": "dashed", "line-dash-pattern": [8, 3] } },
        { selector: "edge[confidence = 'inferred']", style: { "line-color": conf.inferred, "target-arrow-color": conf.inferred, "line-style": "dotted" } },
      ],
      layout: layout === "concentric"
        ? { name: "concentric", concentric: (n: cytoscape.NodeSingular) => (n.data("focus") ? 10 : 1), levelWidth: () => 1, animate: false }
        : layout === "breadthfirst"
          ? { name: "breadthfirst", directed: true, spacingFactor: 1.2, animate: false }
          : { name: "cose", animate: false, nodeRepulsion: () => 9000, idealEdgeLength: () => 90, randomize: false },
    });
    instance.on("tap", "node", (ev) => {
      const id = Number((ev.target.id() as string).slice(1));
      const n = byId.get(id);
      if (n) select.current(n);
    });
    cy.current = instance;
    return () => {
      instance.destroy();
      cy.current = null;
    };
  }, [nodes, edges, focusId, layout]);

  return <div ref={container} className="h-full w-full" role="img" aria-label={`Graph of ${nodes.length} nodes and ${edges.length} relationships. The same information is listed in the panel beside it.`} />;
}

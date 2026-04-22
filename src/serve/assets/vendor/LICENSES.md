# Third-party JavaScript bundles

These files are vendored into the cqs binary via `include_bytes!`. They
ship under their original licenses (compatible with cqs's own license).

| File | Source | License |
|---|---|---|
| `cytoscape.min.js` (v3.30.4) | https://github.com/cytoscape/cytoscape.js | MIT |
| `dagre.min.js` (v0.8.5) | https://github.com/dagrejs/dagre | MIT |
| `cytoscape-dagre.min.js` (v2.5.0) | https://github.com/cytoscape/cytoscape.js-dagre | MIT |
| `three.min.js` (v0.149.0) | https://github.com/mrdoob/three.js | MIT |
| `3d-force-graph.min.js` (v1.77.0) | https://github.com/vasturiano/3d-force-graph | MIT |

Three.js 0.149.0 is the last release with a UMD/IIFE bundle suitable for
`<script>` tag inclusion. Newer Three.js (0.150+) is ESM-only and would
require a bundler step — out of scope for cqs serve's "no build step"
property.

To refresh:

```bash
curl -sSL -o cytoscape.min.js       https://cdn.jsdelivr.net/npm/cytoscape@3.30.4/dist/cytoscape.min.js
curl -sSL -o dagre.min.js            https://cdn.jsdelivr.net/npm/dagre@0.8.5/dist/dagre.min.js
curl -sSL -o cytoscape-dagre.min.js  https://cdn.jsdelivr.net/npm/cytoscape-dagre@2.5.0/cytoscape-dagre.js
curl -sSL -o three.min.js            https://unpkg.com/three@0.149.0/build/three.min.js
curl -sSL -o 3d-force-graph.min.js   https://cdn.jsdelivr.net/npm/3d-force-graph@1.77.0/dist/3d-force-graph.min.js
```

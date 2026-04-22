# Third-party JavaScript bundles

These files are vendored into the cqs binary via `include_bytes!`. They
ship under their original licenses (compatible with cqs's own license).

| File | Source | License |
|---|---|---|
| `cytoscape.min.js` (v3.30.4) | https://github.com/cytoscape/cytoscape.js | MIT |
| `dagre.min.js` (v0.8.5) | https://github.com/dagrejs/dagre | MIT |
| `cytoscape-dagre.min.js` (v2.5.0) | https://github.com/cytoscape/cytoscape.js-dagre | MIT |

To refresh:

```bash
curl -sSL -o cytoscape.min.js     https://cdn.jsdelivr.net/npm/cytoscape@3.30.4/dist/cytoscape.min.js
curl -sSL -o dagre.min.js          https://cdn.jsdelivr.net/npm/dagre@0.8.5/dist/dagre.min.js
curl -sSL -o cytoscape-dagre.min.js https://cdn.jsdelivr.net/npm/cytoscape-dagre@2.5.0/cytoscape-dagre.js
```

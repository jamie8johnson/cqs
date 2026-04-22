// cqs serve — v1 stub frontend.
//
// Step 1 wires the stats badge against /api/stats and the search box
// against /api/search. Step 3 swaps the #cy-container placeholder for
// a Cytoscape.js canvas backed by /api/graph.

(function () {
  "use strict";

  const $stats = document.getElementById("stats-badge");
  const $search = document.getElementById("search");
  const $sidebar = document.getElementById("sidebar");

  async function loadStats() {
    try {
      const resp = await fetch("/api/stats");
      if (!resp.ok) {
        $stats.textContent = `stats: HTTP ${resp.status}`;
        return;
      }
      const s = await resp.json();
      $stats.textContent = `${s.total_chunks.toLocaleString()} chunks · ${s.call_edges.toLocaleString()} edges`;
    } catch (e) {
      $stats.textContent = `stats: ${e.message}`;
    }
  }

  let searchTimer = null;
  function onSearchInput() {
    clearTimeout(searchTimer);
    searchTimer = setTimeout(runSearch, 200);
  }

  async function runSearch() {
    const q = $search.value.trim();
    if (q.length < 2) {
      $sidebar.innerHTML = `<p class="hint">Type 2+ characters to search.</p>`;
      return;
    }
    try {
      const resp = await fetch(`/api/search?q=${encodeURIComponent(q)}&limit=20`);
      if (!resp.ok) {
        $sidebar.innerHTML = `<p class="hint">search HTTP ${resp.status}</p>`;
        return;
      }
      const data = await resp.json();
      if (!data.matches || data.matches.length === 0) {
        $sidebar.innerHTML = `<p class="hint">no matches for "${escapeHtml(q)}"</p>`;
        return;
      }
      const items = data.matches
        .map(m => `<li><strong>${escapeHtml(m.name)}</strong><br><small>${escapeHtml(m.file)}:${m.line_start}</small></li>`)
        .join("");
      $sidebar.innerHTML = `<p class="hint">${data.matches.length} match${data.matches.length === 1 ? "" : "es"}:</p><ul>${items}</ul>`;
    } catch (e) {
      $sidebar.innerHTML = `<p class="hint">search error: ${escapeHtml(e.message)}</p>`;
    }
  }

  function escapeHtml(s) {
    return String(s)
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;")
      .replace(/"/g, "&quot;")
      .replace(/'/g, "&#039;");
  }

  $search.addEventListener("input", onSearchInput);
  loadStats();
})();

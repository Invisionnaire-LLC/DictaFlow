// Helpers shared by every DictaFlow window. Load before the page script.
window.DF = (() => {
  /** Resolve with `window.__TAURI__` once the IPC bridge is available (null after ~5 s). */
  async function tauri() {
    for (let i = 0; i < 50; i++) {
      if (window.__TAURI__?.core?.invoke) return window.__TAURI__;
      await new Promise((r) => setTimeout(r, 100));
    }
    return null;
  }

  function escapeHtml(s) {
    return String(s)
      .replaceAll("&", "&amp;")
      .replaceAll("<", "&lt;")
      .replaceAll(">", "&gt;")
      .replaceAll('"', "&quot;");
  }

  const IS_MAC = /Mac/.test(navigator.platform);
  const MAC_KEYS = { Alt: "Option", Meta: "Command" };

  /** "Option + Space" on macOS, "Alt + Space" elsewhere; `KeyD` → `D`. */
  function displayHotkey(h) {
    if (!h || !h.code) return "-";
    const parts = (h.modifiers || []).map((m) => (IS_MAC && MAC_KEYS[m]) || m);
    parts.push(h.code.replace(/^(Key|Digit)/, ""));
    return parts.join(" + ");
  }

  /** Show a status line; `kind` is "", "ok", or "err". */
  function setMsg(el, text = "", kind = "") {
    el.textContent = text;
    el.className = kind ? `msg ${kind}` : "msg";
  }

  return { tauri, escapeHtml, displayHotkey, setMsg };
})();

(async () => {
  for (let i = 0; i < 50; i++) {
    if (window.__TAURI__?.core?.invoke) break;
    await new Promise((r) => setTimeout(r, 100));
  }
  const invoke = window.__TAURI__?.core?.invoke;
  if (!invoke) return;

  function displayHotkey(h) {
    const labels = { Alt: "Option", Control: "Control", Shift: "Shift", Meta: "Command" };
    const parts = (h.modifiers || []).map((m) => labels[m] || m);
    parts.push(h.code === "Space" ? "Space" : h.code.replace(/^Key/, ""));
    return parts.join(" + ");
  }

  try {
    const info = await invoke("get_build_info");
    document.getElementById("versionLine").textContent = info.version;
    document.getElementById("buildHash").textContent = info.git_hash;
  } catch (_) {}

  invoke("get_ollama_model")
    .then((model) => {
      document.getElementById("ollamaModel").textContent = model;
    })
    .catch(() => {});

  invoke("get_hotkey")
    .then((h) => {
      document.getElementById("hotkeyLine").textContent = `Hold ${displayHotkey(h)} to dictate`;
    })
    .catch(() => {
      document.getElementById("hotkeyLine").textContent = "Configure in Settings";
    });
})();

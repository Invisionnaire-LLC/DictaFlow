(async () => {
  const tauri = await DF.tauri();
  if (!tauri) return;
  const { invoke } = tauri.core;

  try {
    const info = await invoke("get_build_info");
    document.getElementById("versionLine").textContent = info.version;
    document.getElementById("buildHash").textContent = info.git_hash;
  } catch (_) {}

  invoke("get_cleanup_config")
    .then((cfg) => {
      const model = cfg.resolved_model || cfg.model || "";
      const label = cfg.backend === "lmstudio" ? `LM Studio: ${model}` : model;
      document.getElementById("ollamaModel").textContent = label;
    })
    .catch(() => {});

  invoke("get_hotkey")
    .then((h) => {
      document.getElementById("hotkeyLine").textContent = `Hold ${DF.displayHotkey(h)} to dictate`;
    })
    .catch(() => {
      document.getElementById("hotkeyLine").textContent = "Configure in Settings";
    });
})();

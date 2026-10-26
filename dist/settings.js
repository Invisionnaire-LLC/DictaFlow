(async () => {
    const tauri = await DF.tauri();
    if (!tauri) return;
    const { invoke } = tauri.core;
    const { setMsg, escapeHtml, displayHotkey } = DF;

    const navButtons = document.querySelectorAll(".nav-btn");
    const panels = document.querySelectorAll(".panel");

    navButtons.forEach((btn) => {
      btn.addEventListener("click", () => {
        const id = btn.dataset.panel;
        navButtons.forEach((b) => b.removeAttribute("aria-current"));
        btn.setAttribute("aria-current", "page");
        panels.forEach((panel) => {
          panel.classList.toggle("active", panel.id === `panel-${id}`);
        });
        if (id === "vocabulary") loadVocabularyPanel();
      });
    });

    const currentEl = document.getElementById("current");
    const captureEl = document.getElementById("capture");
    const hkMsgEl = document.getElementById("hkMsg");
    const saveBtn = document.getElementById("save");
    const closeBtn = document.getElementById("close");
    const cleanupBackendEl = document.getElementById("cleanupBackend");
    const lmstudioUrlFieldEl = document.getElementById("lmstudioUrlField");
    const lmstudioBaseUrlEl = document.getElementById("lmstudioBaseUrl");
    const lmstudioUrlSaveBtn = document.getElementById("lmstudioUrlSave");
    const cleanupModelEl = document.getElementById("cleanupModel");
    const cleanupToggleEl = document.getElementById("cleanupToggle");
    const cleanupListEl = document.getElementById("cleanupList");
    const cleanupMsgEl = document.getElementById("cleanupMsg");
    const cleanupModelLabelEl = document.getElementById("cleanupModelLabel");
    const cleanupHintEl = document.getElementById("cleanupHint");
    const cleanupSaveBtn = document.getElementById("cleanupSave");
    const cleanupRefreshBtn = document.getElementById("cleanupRefresh");
    const launchAtLoginEl = document.getElementById("launchAtLogin");
    const loginMsgEl = document.getElementById("loginMsg");

    const hudStyleEl = document.getElementById("hudStyle");
    const hudStyleMsgEl = document.getElementById("hudStyleMsg");
    const hudAlwaysOnTopEl = document.getElementById("hudAlwaysOnTop");
    const liveTranscriptEl = document.getElementById("liveTranscript");
    const resetHudPositionBtn = document.getElementById("resetHudPosition");
    const cursorHudStatusEl = document.getElementById("cursorHudStatus");
    const buildVersionEl = document.getElementById("buildVersion");
    const buildHashEl = document.getElementById("buildHash");

    const dictListEl = document.getElementById("dictList");
    const dictMsgEl = document.getElementById("dictMsg");
    const dictRefreshBtn = document.getElementById("dictRefresh");
    const corrFromEl = document.getElementById("corrFrom");
    const corrToEl = document.getElementById("corrTo");
    const corrAddBtn = document.getElementById("corrAdd");
    const vocabEl = document.getElementById("vocab");
    const vocabMsgEl = document.getElementById("vocabMsg");
    const vocabSaveBtn = document.getElementById("vocabSave");
    let vocabCurrent = "";

    let pending = null;
    let currentCleanupModel = "";
    let cleanupBackend = "ollama";

    async function loadCurrent() {
      try {
        const h = await invoke("get_hotkey");
        currentEl.textContent = displayHotkey(h);
      } catch (e) {
        currentEl.textContent = "(error)";
        console.error(e);
      }
    }
    loadCurrent();

    function formatBytes(bytes) {
      if (!bytes) return "";
      const units = ["B", "KB", "MB", "GB"];
      let value = bytes;
      let unit = 0;
      while (value >= 1024 && unit < units.length - 1) {
        value /= 1024;
        unit += 1;
      }
      return `${value.toFixed(unit === 0 ? 0 : 1)} ${units[unit]}`;
    }

    const setCleanupMessage = (text, kind) => setMsg(cleanupMsgEl, text, kind);
    const setLoginMessage = (text, kind) => setMsg(loginMsgEl, text, kind);
    const setDictMessage = (text, kind) => setMsg(dictMsgEl, text, kind);
    const setHudMessage = (text, kind) => setMsg(hudStyleMsgEl, text, kind);

    function cleanupIsLmStudio() {
      return cleanupBackendEl.value === "lmstudio";
    }

    const OLLAMA_DEFAULT_ITEM = { value: "gemma3:1b", meta: "DictaFlow default" };
    let comboItems = [OLLAMA_DEFAULT_ITEM];

    function renderComboList() {
      const selected = cleanupModelEl.value.trim();
      cleanupListEl.replaceChildren();
      for (const item of comboItems) {
        const row = document.createElement("div");
        row.className = "combo-item";
        row.setAttribute("role", "option");
        if (item.value === selected) row.classList.add("selected");
        const name = document.createElement("span");
        name.textContent = item.value;
        row.appendChild(name);
        if (item.meta) {
          const meta = document.createElement("span");
          meta.className = "meta";
          meta.textContent = item.meta;
          row.appendChild(meta);
        }
        row.addEventListener("mousedown", (e) => {
          e.preventDefault();
          cleanupModelEl.value = item.value;
          onCleanupModelInput();
          closeComboList();
          cleanupModelEl.focus();
        });
        cleanupListEl.appendChild(row);
      }
    }

    function openComboList() {
      renderComboList();
      cleanupListEl.classList.add("open");
      cleanupToggleEl.setAttribute("aria-expanded", "true");
      cleanupModelEl.setAttribute("aria-expanded", "true");
    }

    function closeComboList() {
      cleanupListEl.classList.remove("open");
      cleanupToggleEl.setAttribute("aria-expanded", "false");
      cleanupModelEl.setAttribute("aria-expanded", "false");
    }

    // Set the hint via safe DOM nodes (no innerHTML).
    function setCleanupHint(lm) {
      cleanupHintEl.replaceChildren();
      if (lm) {
        cleanupHintEl.textContent =
          "Pick a model loaded in LM Studio, or leave blank to use the first available. The server must be running with a model loaded.";
      } else {
        cleanupHintEl.append(
          "Type any Ollama model, or open the list to pick from installed ones. "
        );
        const b = document.createElement("b");
        b.textContent = "gemma3:1b";
        cleanupHintEl.append(b, " is always available as the DictaFlow default.");
      }
    }

    // Apply backend-specific labels/placeholders and show the URL field for LM Studio.
    function refreshCleanupFields() {
      const lm = cleanupIsLmStudio();
      lmstudioUrlFieldEl.style.display = lm ? "block" : "none";
      cleanupModelLabelEl.textContent = lm ? "LM Studio model" : "Model name";
      cleanupModelEl.placeholder = lm ? "(first available)" : "gemma3:1b";
      setCleanupHint(lm);
    }

    // `cfg` is passed in when the caller already has a fresh cleanup config,
    // saving a round-trip (with LM Studio, each one can hit the server).
    async function loadCleanupModels(cfg = null) {
      cleanupModelEl.disabled = true;
      cleanupSaveBtn.disabled = true;
      cleanupModelEl.placeholder = "Loading…";
      setCleanupMessage("");
      const lm = cleanupIsLmStudio();
      const defaultItems = lm ? [] : [OLLAMA_DEFAULT_ITEM];
      let current = cfg?.model || "";
      try {
        const [freshCfg, models] = await Promise.all([
          cfg ?? invoke("get_cleanup_config"),
          invoke("list_cleanup_models"),
        ]);
        current = freshCfg.model || "";
        const names = new Set(defaultItems.map((i) => i.value));
        comboItems = defaultItems.slice();
        for (const model of models) {
          if (names.has(model.name)) continue;
          names.add(model.name);
          comboItems.push({ value: model.name, meta: lm ? "" : formatBytes(model.size) });
        }
        if (current && !names.has(current)) {
          setCleanupMessage(`Current: ${current} (manual model)`, "");
        } else if (!models.length) {
          setCleanupMessage(
            lm
              ? "No models reported. Load a model in LM Studio, then Refresh."
              : "Using DictaFlow default. You can also type any Ollama model name manually.",
            lm ? "" : "ok"
          );
        } else {
          setCleanupMessage(current ? `Current: ${current}` : "Using first available.", "ok");
        }
      } catch (err) {
        if (!cfg) {
          current = await invoke("get_cleanup_config").then((c) => c.model || "").catch(() => "");
        }
        comboItems = defaultItems.slice();
        setCleanupMessage(`${String(err)}. You can still type and save a model name manually.`, "err");
      }
      currentCleanupModel = current;
      cleanupModelEl.value = current;
      cleanupModelEl.placeholder = lm ? "(first available)" : "gemma3:1b";
      cleanupModelEl.disabled = false;
      cleanupSaveBtn.disabled = true;
      renderComboList();
    }

    async function loadCleanupConfig() {
      let cfg = null;
      try {
        cfg = await invoke("get_cleanup_config");
        cleanupBackend = cfg.backend || "ollama";
        cleanupBackendEl.value = cleanupBackend;
        lmstudioBaseUrlEl.value = cfg.base_url || "";
      } catch (err) {
        cleanupBackendEl.value = "ollama";
      }
      refreshCleanupFields();
      await loadCleanupModels(cfg);
    }
    loadCleanupConfig();

    async function loadLaunchAtLogin() {
      try {
        const supported = await invoke("launch_at_login_supported");
        const launchRow = document.getElementById("launchAtLoginRow");
        const masNote = document.getElementById("masLoginNote");
        if (!supported) {
          launchRow.hidden = true;
          masNote.hidden = false;
          return;
        }
        launchRow.hidden = false;
        masNote.hidden = true;
        launchAtLoginEl.checked = await invoke("get_launch_at_login");
      } catch (err) {
        setLoginMessage(String(err), "err");
      }
    }
    loadLaunchAtLogin();

    const DICT_EMPTY_HTML =
      '<div class="dict-empty">No corrections yet.<br>Add one above or fix pasted text after dictating.</div>';

    async function loadDictionary() {
      dictListEl.innerHTML = '<div class="dict-empty">Loading…</div>';
      setDictMessage("");
      try {
        const items = await invoke("list_dictionary");
        if (!items.length) {
          dictListEl.innerHTML = DICT_EMPTY_HTML;
          return;
        }
        dictListEl.innerHTML = "";
        for (const item of items) {
          const row = document.createElement("div");
          row.className = "dict-item";
          const pair = document.createElement("div");
          pair.className = "dict-pair";
          pair.innerHTML = `<span class="from">${escapeHtml(item.from)}</span><span class="arrow">→</span><span class="to">${escapeHtml(item.to)}</span>`;
          const del = document.createElement("button");
          del.type = "button";
          del.className = "dict-delete";
          del.textContent = "Remove";
          del.addEventListener("click", async () => {
            del.disabled = true;
            row.remove();
            try {
              await invoke("delete_dictionary_entry", { from: item.from });
              setDictMessage(`Removed ${item.from}.`, "ok");
              if (!dictListEl.childElementCount) {
                dictListEl.innerHTML = DICT_EMPTY_HTML;
              }
            } catch (err) {
              setDictMessage(String(err), "err");
              await loadDictionary();
            }
          });
          row.appendChild(pair);
          row.appendChild(del);
          dictListEl.appendChild(row);
        }
      } catch (err) {
        dictListEl.innerHTML = '<div class="dict-empty">Could not load corrections.</div>';
        setDictMessage(String(err), "err");
      }
    }

    async function loadVocabWords() {
      try {
        vocabCurrent = await invoke("get_vocabulary");
        vocabEl.value = vocabCurrent;
        vocabSaveBtn.disabled = true;
      } catch (err) {
        console.error(err);
      }
    }

    function loadVocabularyPanel() {
      loadVocabWords();
      loadDictionary();
    }

    dictRefreshBtn.addEventListener("click", loadDictionary);

    corrAddBtn.addEventListener("click", async () => {
      const from = corrFromEl.value.trim();
      const to = corrToEl.value.trim();
      if (!from || !to) {
        setDictMessage("Enter both heard and wanted text.", "err");
        return;
      }
      corrAddBtn.disabled = true;
      setDictMessage("");
      try {
        await invoke("add_dictionary_entry", { from, to });
        corrFromEl.value = "";
        corrToEl.value = "";
        setDictMessage(`Added: ${from} → ${to}`, "ok");
        await loadDictionary();
      } catch (err) {
        setDictMessage(String(err), "err");
      } finally {
        corrAddBtn.disabled = false;
      }
    });

    vocabEl.addEventListener("input", () => {
      vocabSaveBtn.disabled = vocabEl.value.trim() === vocabCurrent.trim();
    });

    vocabSaveBtn.addEventListener("click", async () => {
      vocabSaveBtn.disabled = true;
      setMsg(vocabMsgEl);
      try {
        const saved = await invoke("set_vocabulary", { words: vocabEl.value });
        vocabCurrent = saved;
        vocabEl.value = saved;
        setMsg(vocabMsgEl, "Saved.", "ok");
      } catch (err) {
        setMsg(vocabMsgEl, String(err), "err");
        vocabSaveBtn.disabled = false;
      }
    });

    const MODIFIER_CODES = new Set([
      "ControlLeft", "ControlRight",
      "ShiftLeft", "ShiftRight",
      "AltLeft", "AltRight",
      "MetaLeft", "MetaRight",
    ]);

    function collectModifiers(e) {
      const out = [];
      if (e.ctrlKey) out.push("Control");
      if (e.altKey) out.push("Alt");
      if (e.shiftKey) out.push("Shift");
      if (e.metaKey) out.push("Meta");
      return out;
    }

    function onKeyDown(e) {
      e.preventDefault();
      e.stopPropagation();
      if (MODIFIER_CODES.has(e.code)) {
        const mods = collectModifiers(e);
        captureEl.textContent = mods.length ? displayHotkey({ code: "…", modifiers: mods }) : "…";
        return;
      }
      const modifiers = collectModifiers(e);
      pending = { code: e.code, modifiers };
      captureEl.textContent = displayHotkey(pending);
      captureEl.classList.remove("recording");
      saveBtn.disabled = false;
      setMsg(hkMsgEl);
      captureEl.blur();
    }

    captureEl.addEventListener("focus", () => {
      captureEl.classList.add("recording");
      captureEl.textContent = "Press keys…";
    });
    captureEl.addEventListener("blur", () => {
      captureEl.classList.remove("recording");
      if (!pending) captureEl.textContent = "Click here, then press keys…";
    });
    captureEl.addEventListener("click", () => captureEl.focus());
    captureEl.addEventListener("keydown", onKeyDown);

    saveBtn.addEventListener("click", async () => {
      if (!pending) return;
      saveBtn.disabled = true;
      setMsg(hkMsgEl);
      try {
        const saved = await invoke("set_hotkey", { hotkey: pending });
        currentEl.textContent = displayHotkey(saved);
        setMsg(hkMsgEl, "Saved.", "ok");
        pending = null;
        captureEl.textContent = "Click here, then press keys…";
        saveBtn.disabled = true;
      } catch (err) {
        setMsg(hkMsgEl, String(err), "err");
        saveBtn.disabled = false;
      }
    });

    function onCleanupModelInput() {
      const value = cleanupModelEl.value.trim();
      // LM Studio allows an empty model (= first available); Ollama requires one.
      const sameAsCurrent = value === currentCleanupModel;
      cleanupSaveBtn.disabled = sameAsCurrent || (!cleanupIsLmStudio() && !value);
      setCleanupMessage(
        sameAsCurrent && value ? `Current: ${currentCleanupModel}` : "",
        sameAsCurrent && value ? "ok" : ""
      );
    }
    cleanupModelEl.addEventListener("input", onCleanupModelInput);
    cleanupModelEl.addEventListener("focus", openComboList);
    cleanupToggleEl.addEventListener("click", (e) => {
      e.preventDefault();
      if (cleanupListEl.classList.contains("open")) {
        closeComboList();
      } else {
        openComboList();
        cleanupModelEl.focus();
      }
    });
    document.addEventListener("click", (e) => {
      if (!e.target.closest(".combo-wrap")) closeComboList();
    });
    document.addEventListener("keydown", (e) => {
      if (e.key === "Escape") closeComboList();
    });

    // Switching engine persists immediately and reloads that backend's models.
    cleanupBackendEl.addEventListener("change", async () => {
      setCleanupMessage("Switching…");
      try {
        const cfg = await invoke("set_cleanup_backend", {
          backend: cleanupBackendEl.value,
          baseUrl: cleanupIsLmStudio() ? lmstudioBaseUrlEl.value.trim() : null,
        });
        cleanupBackend = cfg.backend;
        cleanupBackendEl.value = cfg.backend;
        lmstudioBaseUrlEl.value = cfg.base_url || "";
        refreshCleanupFields();
        await loadCleanupModels(cfg);
      } catch (err) {
        setCleanupMessage(String(err), "err");
      }
    });

    // Applying a new LM Studio URL re-points the backend, then reloads models.
    lmstudioUrlSaveBtn.addEventListener("click", async () => {
      setCleanupMessage("Applying…");
      try {
        const cfg = await invoke("set_cleanup_backend", {
          backend: "lmstudio",
          baseUrl: lmstudioBaseUrlEl.value.trim(),
        });
        lmstudioBaseUrlEl.value = cfg.base_url || "";
        await loadCleanupModels(cfg);
      } catch (err) {
        setCleanupMessage(String(err), "err");
      }
    });

    cleanupSaveBtn.addEventListener("click", async () => {
      cleanupSaveBtn.disabled = true;
      setCleanupMessage("");
      try {
        const saved = await invoke("set_cleanup_model", { model: cleanupModelEl.value.trim() });
        currentCleanupModel = saved;
        cleanupModelEl.value = saved;
        setCleanupMessage(saved ? `Saved: ${saved}` : "Saved: (first available)", "ok");
      } catch (err) {
        setCleanupMessage(String(err), "err");
        cleanupSaveBtn.disabled = false;
      }
    });

    cleanupRefreshBtn.addEventListener("click", () => loadCleanupModels());

    launchAtLoginEl.addEventListener("change", async () => {
      launchAtLoginEl.disabled = true;
      setLoginMessage("");
      try {
        const enabled = await invoke("set_launch_at_login", { enabled: launchAtLoginEl.checked });
        launchAtLoginEl.checked = enabled;
        setLoginMessage(enabled ? "Enabled." : "Disabled.", "ok");
      } catch (err) {
        launchAtLoginEl.checked = !launchAtLoginEl.checked;
        setLoginMessage(String(err), "err");
      } finally {
        launchAtLoginEl.disabled = false;
      }
    });

    closeBtn.addEventListener("click", () => {
      invoke("close_settings").catch(() => {});
    });

    const axStatusEl = document.getElementById("axStatus");

    async function refreshAx() {
      axStatusEl.textContent = "Checking…";
      axStatusEl.className = "badge pending";
      try {
        const granted = await invoke("check_accessibility");
        if (granted) {
          axStatusEl.textContent = "Granted";
          axStatusEl.className = "badge ok";
        } else {
          axStatusEl.textContent = "Not granted";
          axStatusEl.className = "badge err";
        }
      } catch (e) {
        axStatusEl.textContent = "Error checking";
        axStatusEl.className = "badge err";
      }
    }

    refreshAx();

    document.getElementById("axRequest").addEventListener("click", async () => {
      try {
        await invoke("prompt_accessibility");
        setTimeout(refreshAx, 500);
      } catch (e) {
        console.error(e);
      }
    });
    document.getElementById("axRecheck").addEventListener("click", refreshAx);
    document.getElementById("axOpen").addEventListener("click", () => {
      invoke("open_accessibility_settings").catch(() => {});
    });
    document.getElementById("micOpen").addEventListener("click", () => {
      invoke("open_microphone_settings").catch(() => {});
    });

    const modelDownloadCard = document.getElementById("modelDownloadCard");
    const modelDownloadBar = document.getElementById("modelDownloadBar");
    const modelDownloadMeta = document.getElementById("modelDownloadMeta");

    function setModelDownloadUi(active, pct = 0, message = "") {
      modelDownloadCard.hidden = !active;
      modelDownloadBar.style.width = `${Math.max(0, Math.min(100, pct))}%`;
      modelDownloadMeta.textContent = message || `${pct}%`;
    }

    async function refreshModelDownloadStatus() {
      try {
        const status = await invoke("get_whisper_model_status");
        if (status === "ready") {
          setModelDownloadUi(false);
        } else if (status === "downloading") {
          setModelDownloadUi(true, 0, "Downloading…");
        } else {
          setModelDownloadUi(true, 0, "Waiting to start download…");
        }
      } catch (_) {}
    }
    refreshModelDownloadStatus();

    if (tauri.event?.listen) {
      // Track unlisten handles so re-opening Settings doesn't stack duplicate listeners.
      const unlistens = [];
      const track = (p) => p.then((un) => unlistens.push(un)).catch(() => {});
      track(tauri.event.listen("model_download_start", () => {
        setModelDownloadUi(true, 0, "Starting download…");
      }));
      track(tauri.event.listen("model_download_progress", (e) => {
        const pct = Number(e.payload) || 0;
        setModelDownloadUi(true, pct, `Downloading speech model… ${pct}%`);
      }));
      track(tauri.event.listen("model_download_complete", () => {
        setModelDownloadUi(false);
      }));
      track(tauri.event.listen("model_download_failed", (e) => {
        setModelDownloadUi(true, 0, `Download failed: ${e.payload || "unknown error"}`);
      }));
      window.addEventListener("beforeunload", () => {
        for (const un of unlistens) {
          try { un(); } catch (_) {}
        }
      });
    }

    async function loadHudStyle() {
      try {
        const [style, alwaysOnTop, liveTranscript] = await Promise.all([
          invoke("get_hud_style"),
          invoke("get_hud_always_on_top"),
          invoke("get_live_transcript"),
        ]);
        hudStyleEl.value = style;
        hudAlwaysOnTopEl.checked = alwaysOnTop;
        liveTranscriptEl.checked = liveTranscript;
        refreshCursorHudStatus();
      } catch (err) {
        console.error(err);
      }
    }
    loadHudStyle();

    hudAlwaysOnTopEl.addEventListener("change", async () => {
      setHudMessage();
      try {
        const enabled = await invoke("set_hud_always_on_top", { enabled: hudAlwaysOnTopEl.checked });
        hudAlwaysOnTopEl.checked = enabled;
        setHudMessage(
          enabled ? "HUD stays above other windows." : "HUD only floats up while dictating.",
          "ok"
        );
      } catch (err) {
        setHudMessage(String(err), "err");
      }
    });

    resetHudPositionBtn.addEventListener("click", async () => {
      setHudMessage();
      resetHudPositionBtn.disabled = true;
      try {
        await invoke("reset_hud_position");
        setHudMessage("Pill moved to default position.", "ok");
      } catch (err) {
        setHudMessage(String(err), "err");
      } finally {
        resetHudPositionBtn.disabled = false;
      }
    });

    liveTranscriptEl.addEventListener("change", async () => {
      setHudMessage();
      try {
        const enabled = await invoke("set_live_transcript", { enabled: liveTranscriptEl.checked });
        liveTranscriptEl.checked = enabled;
        setHudMessage(enabled ? "Live transcript on." : "Live transcript off.", "ok");
      } catch (err) {
        setHudMessage(String(err), "err");
      }
    });

    hudStyleEl.addEventListener("change", async () => {
      setHudMessage();
      try {
        const saved = await invoke("set_hud_style", { style: hudStyleEl.value });
        hudStyleEl.value = saved;
        const label = { notch: "Notch", cursor: "Cursor" }[saved] || "Classic";
        setHudMessage(`${label} style.`, "ok");
        refreshCursorHudStatus();
      } catch (err) {
        setHudMessage(String(err), "err");
      }
    });

    // --- Transcription backend ---

    const asrLanguageEl = document.getElementById("asrLanguage");
    const asrBackendEl = document.getElementById("asrBackend");
    const asrCloudFieldsEl = document.getElementById("asrCloudFields");
    const asrApiKeyEl = document.getElementById("asrApiKey");
    const asrModelEl = document.getElementById("asrModel");
    const asrHintEl = document.getElementById("asrHint");
    const asrMsgEl = document.getElementById("asrMsg");
    const asrSaveBtn = document.getElementById("asrSave");
    const autoSubmitEl = document.getElementById("autoSubmit");

    let asrCurrent = { backend: "local", model: "", hasKey: false };

    const ASR_HINTS = {
      groq: "Get a key at console.groq.com. Default model: whisper-large-v3.",
      openai: "Get a key at platform.openai.com. Default model: gpt-4o-transcribe.",
    };
    const ASR_MODELS = {
      groq: ["whisper-large-v3", "whisper-large-v3-turbo", "distil-whisper-large-v3-en"],
      openai: ["gpt-4o-transcribe", "gpt-4o-mini-transcribe", "whisper-1"],
    };

    const setAsrMessage = (text, kind) => setMsg(asrMsgEl, text, kind);

    function asrIsCloud() {
      return asrBackendEl.value !== "local";
    }

    function populateAsrModels(provider, selected) {
      asrModelEl.innerHTML = "";
      const list = ASR_MODELS[provider] || [];
      for (const m of list) {
        const opt = document.createElement("option");
        opt.value = m;
        opt.textContent = m;
        asrModelEl.appendChild(opt);
      }
      asrModelEl.value = selected && list.includes(selected) ? selected : (list[0] || "");
    }

    function asrDirty() {
      const model = asrIsCloud() ? asrModelEl.value : "";
      const typedNewKey = asrApiKeyEl.value.trim() !== "";
      return (
        asrBackendEl.value !== asrCurrent.backend ||
        model !== (asrCurrent.model || "") ||
        typedNewKey
      );
    }

    function updateAsrSaveState() {
      // A stored key counts — blank field then means "keep existing".
      const needsKey = asrIsCloud() && !asrCurrent.hasKey && !asrApiKeyEl.value.trim();
      asrSaveBtn.disabled = needsKey || !asrDirty();
    }

    function refreshAsrFields() {
      const cloud = asrIsCloud();
      asrCloudFieldsEl.style.display = cloud ? "block" : "none";
      if (cloud) {
        populateAsrModels(asrBackendEl.value, asrModelEl.value);
        asrHintEl.textContent = ASR_HINTS[asrBackendEl.value] || "";
      }
      updateAsrSaveState();
    }

    async function loadAsrConfig() {
      try {
        const cfg = await invoke("get_transcription_config");
        const provider = cfg.backend === "local" ? "groq" : cfg.backend;
        populateAsrModels(provider, cfg.model || cfg.resolved_model);
        asrBackendEl.value = cfg.backend;
        // The raw key never leaves the backend — show only a masked hint as placeholder.
        asrApiKeyEl.value = "";
        asrApiKeyEl.placeholder = cfg.has_key
          ? `${cfg.api_key_hint} · saved (leave blank to keep)`
          : "Paste your API key";
        asrCurrent = {
          backend: cfg.backend,
          model: cfg.backend === "local" ? "" : asrModelEl.value,
          hasKey: !!cfg.has_key,
        };
        refreshAsrFields();
      } catch (err) {
        setAsrMessage(String(err), "err");
      }
    }
    loadAsrConfig();

    asrBackendEl.addEventListener("change", refreshAsrFields);
    asrApiKeyEl.addEventListener("input", updateAsrSaveState);
    asrModelEl.addEventListener("change", updateAsrSaveState);

    asrSaveBtn.addEventListener("click", async () => {
      asrSaveBtn.disabled = true;
      setAsrMessage("Saving…");
      try {
        const backend = await invoke("set_transcription_config", {
          backend: asrBackendEl.value,
          apiKey: asrApiKeyEl.value.trim(),
          model: asrIsCloud() ? asrModelEl.value : "",
        });
        setAsrMessage(
          backend === "local"
            ? "Using local Whisper (offline)."
            : `Using ${backend} API. Audio will be sent to the cloud on each dictation.`,
          "ok"
        );
        // Reload from the backend so state reflects what was actually stored
        // (confirmed model, key field cleared, has_key refreshed).
        await loadAsrConfig();
      } catch (err) {
        setAsrMessage(String(err), "err");
        updateAsrSaveState();
      }
    });

    // Language (applies to local + cloud, saves instantly)
    async function loadAsrLanguage() {
      try {
        asrLanguageEl.value = await invoke("get_transcription_language");
      } catch (err) {
        console.error(err);
      }
    }
    loadAsrLanguage();
    asrLanguageEl.addEventListener("change", async () => {
      try {
        const saved = await invoke("set_transcription_language", { language: asrLanguageEl.value });
        asrLanguageEl.value = saved;
        setAsrMessage("Language updated.", "ok");
      } catch (err) {
        setAsrMessage(String(err), "err");
      }
    });

    // Auto-submit (press Enter after paste)
    async function loadAutoSubmit() {
      try {
        autoSubmitEl.checked = await invoke("get_auto_submit");
      } catch (err) {
        console.error(err);
      }
    }
    loadAutoSubmit();
    autoSubmitEl.addEventListener("change", async () => {
      try {
        const enabled = await invoke("set_auto_submit", { enabled: autoSubmitEl.checked });
        autoSubmitEl.checked = enabled;
        setAsrMessage(enabled ? "Auto-send on: Enter is pressed after paste." : "Auto-send off.", "ok");
      } catch (err) {
        autoSubmitEl.checked = !autoSubmitEl.checked;
        setAsrMessage(String(err), "err");
      }
    });

    async function refreshCursorHudStatus() {
      const style = hudStyleEl.value;
      if (style !== "cursor") {
        cursorHudStatusEl.hidden = true;
        return;
      }
      try {
        const status = await invoke("get_cursor_hud_status");
        cursorHudStatusEl.hidden = false;
        cursorHudStatusEl.textContent =
          status === "ready"
            ? "Cursor tracking: ready"
            : "Cursor tracking: unavailable — pill may appear on the wrong screen";
      } catch {
        cursorHudStatusEl.hidden = true;
      }
    }

    async function loadBuildInfo() {
      try {
        const info = await invoke("get_build_info");
        buildVersionEl.textContent = info.version;
        buildHashEl.textContent = info.git_hash;
      } catch {
        buildVersionEl.textContent = "?";
        buildHashEl.textContent = "?";
      }
    }

    loadBuildInfo();
})();

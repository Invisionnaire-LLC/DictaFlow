    const body = document.body;
    const previewEl = document.getElementById('preview');
    const previewTextEl = previewEl.querySelector('span');
    const downloadLabelEl = document.getElementById('download-label');
    const bars = document.querySelectorAll('.bar');
    let currentRms = 0;
    let smoothed = 0;

    function setPreview(text) {
      const trimmed = String(text || '').trim();
      if (!trimmed) {
        previewTextEl.textContent = 'Listening…';
        previewEl.classList.add('empty');
        previewEl.scrollLeft = 0;
        return;
      }
      previewTextEl.textContent = trimmed;
      previewEl.classList.remove('empty');
      requestAnimationFrame(() => {
        previewEl.scrollLeft = previewEl.scrollWidth;
      });
    }

    // The status label shows model-download progress and short notices.
    let statusTimer = null;
    function flashStatus(text, ms = 2500) {
      clearTimeout(statusTimer);
      body.dataset.downloading = 'on';
      downloadLabelEl.textContent = text;
      statusTimer = setTimeout(() => {
        body.dataset.downloading = '';
        downloadLabelEl.textContent = '';
      }, ms);
    }

    function setDownloadProgress(pct) {
      const value = Math.max(0, Math.min(100, Number(pct) || 0));
      if (value <= 0) {
        body.dataset.downloading = '';
        downloadLabelEl.textContent = '';
        return;
      }
      body.dataset.layout = 'expanded';
      if (value >= 100) {
        flashStatus('Ready', 1800);
        return;
      }
      body.dataset.downloading = 'on';
      downloadLabelEl.textContent = `DL ${value}%`;
    }

    // The waveform only moves while recording, so the frame loop runs only then
    // (and only while the HUD is visible); otherwise the bars rest at BAR_BASE.
    const BAR_BASE = 6;
    const BAR_GROWTH = 22;
    let barsRaf = null;
    function animateBars() {
      smoothed += (currentRms - smoothed) * 0.35;
      const boost = Math.max(smoothed, 0.05);
      const now = Date.now();
      bars.forEach((bar, i) => {
        const wiggle = (Math.sin(now / 180 + i * 0.7) + 1) / 2;
        const h = BAR_BASE + boost * BAR_GROWTH * (0.4 + wiggle * 0.6);
        bar.style.height = h.toFixed(1) + 'px';
      });
      barsRaf = requestAnimationFrame(animateBars);
    }
    function syncBars() {
      const run = body.dataset.state === 'recording' && !document.hidden;
      if (run && barsRaf === null) {
        animateBars();
      } else if (!run && barsRaf !== null) {
        cancelAnimationFrame(barsRaf);
        barsRaf = null;
        smoothed = 0;
        bars.forEach((bar) => { bar.style.height = ''; });
      }
    }
    document.addEventListener('visibilitychange', syncBars);

    function setLiveTranscript(enabled) {
      body.dataset.liveTranscript = enabled ? 'on' : 'off';
      if (!enabled) {
        previewTextEl.textContent = '';
        previewEl.classList.add('empty');
        previewEl.scrollLeft = 0;
      }
    }

    function applyHudLayout(expanded) {
      const transcriptOn = body.dataset.liveTranscript === 'on';
      const style = body.dataset.style;
      const shouldLayout = style === 'notch' || ((style === 'classic' || style === 'cursor') && transcriptOn);
      if (!shouldLayout) return;
      body.dataset.layout = expanded ? 'expanded' : 'compact';
      if (expanded && transcriptOn) {
        previewTextEl.textContent = 'Listening…';
        previewEl.classList.add('empty');
        previewEl.scrollLeft = 0;
      } else if (!expanded) {
        previewTextEl.textContent = '';
        previewEl.classList.add('empty');
        previewEl.scrollLeft = 0;
      }
    }

    (async () => {
      const tauri = await DF.tauri();
      if (!tauri) return;
      const { listen } = tauri.event;
      const { invoke } = tauri.core;
      await listen('state', (e) => {
        body.dataset.state = String(e.payload);
        if (e.payload !== 'recording') currentRms = 0;
        syncBars();
      });
      await listen('rms', (e) => {
        currentRms = Math.min(1, Math.max(0, Number(e.payload) || 0));
      });
      await listen('hud_layout', (e) => {
        applyHudLayout(e.payload === 'expanded');
      });
      await listen('partial', (e) => {
        if (body.dataset.liveTranscript !== 'on') return;
        setPreview(e.payload);
      });
      await listen('live_transcript', (e) => {
        setLiveTranscript(Boolean(e.payload));
      });
      await listen('hud_style', (e) => {
        const style = String(e.payload);
        body.dataset.style = style === 'classic' || style === 'cursor' ? style : 'notch';
      });
      // Accessibility is off, so the text was copied instead of pasted.
      await listen('paste_blocked', () => flashStatus('Copied – ⌘V'));
      await listen('model_download_start', () => setDownloadProgress(0));
      await listen('model_download_progress', (e) => setDownloadProgress(e.payload));
      await listen('model_download_complete', () => setDownloadProgress(100));
      await listen('model_download_failed', () => {
        clearTimeout(statusTimer);
        downloadLabelEl.textContent = 'DL failed';
        body.dataset.downloading = 'on';
      });
      try {
        body.dataset.style = await invoke('get_hud_style');
        setLiveTranscript(await invoke('get_live_transcript'));
      } catch (_) {}
    })();

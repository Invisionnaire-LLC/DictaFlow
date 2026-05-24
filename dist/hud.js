    const body = document.body;
    const previewEl = document.getElementById('preview');
    const previewTextEl = previewEl.querySelector('span');
    const learnToastEl = document.getElementById('learn-toast');
    const learnToastTitleEl = learnToastEl.querySelector('.learn-toast-title');
    const learnToastDetailEl = learnToastEl.querySelector('.learn-toast-detail');
    const downloadLabelEl = document.getElementById('download-label');
    const bars = document.querySelectorAll('.bar');
    let currentRms = 0;
    let smoothed = 0;
    let learnToastTimer = null;

    function showLearnToast(from, to) {
      const label = String(from || '').trim();
      const replacement = String(to || '').trim();
      if (!label || !replacement) return;

      learnToastTitleEl.textContent = `"${label}" was learned`;
      learnToastDetailEl.textContent = `Will use "${replacement}" next time`;
      body.dataset.toast = 'learn';
      body.dataset.layout = 'expanded';

      if (learnToastTimer) clearTimeout(learnToastTimer);
      learnToastTimer = setTimeout(() => {
        body.dataset.toast = '';
        if (body.dataset.state === 'idle' && body.dataset.liveTranscript !== 'on') {
          body.dataset.layout = 'compact';
        }
        learnToastTimer = null;
      }, 3200);
    }

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

    function setDownloadProgress(pct) {
      const value = Math.max(0, Math.min(100, Number(pct) || 0));
      if (value <= 0) {
        body.dataset.downloading = '';
        downloadLabelEl.textContent = '';
        return;
      }
      body.dataset.downloading = 'on';
      body.dataset.layout = 'expanded';
      downloadLabelEl.textContent = value >= 100 ? 'Ready' : `DL ${value}%`;
      if (value >= 100) {
        setTimeout(() => {
          body.dataset.downloading = '';
          downloadLabelEl.textContent = '';
        }, 1800);
      }
    }

    function animateBars() {
      smoothed += (currentRms - smoothed) * 0.35;
      const base = 6;
      const maxGrowth = 22;
      const now = Date.now();
      bars.forEach((bar, i) => {
        const phase = (now / 180) + i * 0.7;
        const wiggle = (Math.sin(phase) + 1) / 2;
        const isRecording = body.dataset.state === 'recording';
        const boost = isRecording ? Math.max(smoothed, 0.05) : 0;
        const h = base + boost * maxGrowth * (0.4 + wiggle * 0.6);
        bar.style.height = h.toFixed(1) + 'px';
      });
      requestAnimationFrame(animateBars);
    }
    animateBars();

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

    async function setup() {
      if (!window.__TAURI__?.event) return false;
      const { listen } = window.__TAURI__.event;
      await listen('state', (e) => {
        body.dataset.state = String(e.payload);
        if (e.payload !== 'recording') currentRms = 0;
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
      await listen('word_learned', (e) => {
        const item = e.payload || {};
        if (!item.from || !item.to) return;
        showLearnToast(item.from, item.to);
      });
      await listen('model_download_start', () => setDownloadProgress(0));
      await listen('model_download_progress', (e) => setDownloadProgress(e.payload));
      await listen('model_download_complete', () => setDownloadProgress(100));
      await listen('model_download_failed', () => {
        downloadLabelEl.textContent = 'DL failed';
        body.dataset.downloading = 'on';
      });
      if (window.__TAURI__?.core?.invoke) {
        try {
          body.dataset.style = await window.__TAURI__.core.invoke('get_hud_style');
          setLiveTranscript(await window.__TAURI__.core.invoke('get_live_transcript'));
        } catch (_) {}
      }
      return true;
    }

    async function waitForTauriAndSetup() {
      for (let i = 0; i < 50; i++) {
        if (await setup()) return;
        await new Promise(r => setTimeout(r, 100));
      }
    }

    if (document.readyState === 'loading') {
      document.addEventListener('DOMContentLoaded', waitForTauriAndSetup);
    } else {
      waitForTauriAndSetup();
    }

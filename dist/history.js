    const searchEl = document.getElementById('search');
    const listEl = document.getElementById('list');
    const countEl = document.getElementById('count');
    const msgEl = document.getElementById('msg');

    let debounceTimer = null;
    let entries = [];

    function setMsg(text, kind) {
      msgEl.textContent = text || '';
      msgEl.className = 'msg' + (kind ? ' ' + kind : '');
    }

    function formatTime(iso) {
      try {
        return new Date(iso).toLocaleString();
      } catch {
        return iso;
      }
    }

    function escapeHtml(s) {
      return String(s)
        .replaceAll('&', '&amp;')
        .replaceAll('<', '&lt;')
        .replaceAll('>', '&gt;')
        .replaceAll('"', '&quot;');
    }

    function render() {
      if (!entries.length) {
        listEl.innerHTML = '<div class="empty">No dictations yet. Hold your hotkey and speak.</div>';
        countEl.textContent = '0 entries';
        return;
      }
      countEl.textContent = `${entries.length} entr${entries.length === 1 ? 'y' : 'ies'}`;
      listEl.innerHTML = entries.map((e) => `
        <div class="entry" data-id="${escapeHtml(e.id)}">
          <div class="entry-head">
            <span class="entry-time">${escapeHtml(formatTime(e.created_at))}</span>
            <span class="entry-dur">${e.duration_secs.toFixed(1)}s</span>
          </div>
          <textarea class="entry-edit" data-id="${escapeHtml(e.id)}">${escapeHtml(e.cleaned)}</textarea>
          ${e.raw !== e.cleaned ? `<div class="entry-raw">${escapeHtml(e.raw)}</div>` : ''}
          <div class="entry-actions">
            <button data-action="save-learn" data-id="${escapeHtml(e.id)}">Save and learn</button>
            <button data-action="copy-cleaned" data-id="${escapeHtml(e.id)}">Copy cleaned</button>
            <button data-action="copy-raw" data-id="${escapeHtml(e.id)}">Copy raw</button>
            <button data-action="delete" data-id="${escapeHtml(e.id)}" class="danger">Delete</button>
          </div>
        </div>
      `).join('');
    }

    async function invoke(cmd, args) {
      const { invoke } = window.__TAURI__.core;
      return invoke(cmd, args);
    }

    async function refresh() {
      setMsg('');
      try {
        const query = searchEl.value.trim();
        entries = await invoke('search_history', { query });
        render();
      } catch (err) {
        setMsg(String(err), 'err');
      }
    }

    listEl.addEventListener('click', async (ev) => {
      const btn = ev.target.closest('button[data-action]');
      if (!btn) return;
      const id = btn.dataset.id;
      const entry = entries.find((e) => e.id === id);
      if (!entry) return;

      try {
        if (btn.dataset.action === 'save-learn') {
          const textarea = listEl.querySelector(`textarea.entry-edit[data-id="${CSS.escape(id)}"]`);
          const text = textarea ? textarea.value.trim() : entry.cleaned;
          const learned = await invoke('update_history_entry', { id, cleaned: text });
          if (Array.isArray(learned) && learned.length) {
            setMsg(`Learned: ${learned.map((x) => `${x.from} → ${x.to}`).join(', ')}`, 'ok');
          } else {
            setMsg('Saved.', 'ok');
          }
          await refresh();
        } else if (btn.dataset.action === 'copy-cleaned') {
          await invoke('copy_text', { text: entry.cleaned });
          setMsg('Copied cleaned text.', 'ok');
        } else if (btn.dataset.action === 'copy-raw') {
          await invoke('copy_text', { text: entry.raw });
          setMsg('Copied raw text.', 'ok');
        } else if (btn.dataset.action === 'delete') {
          await invoke('delete_history_entry', { id });
          setMsg('Deleted.', 'ok');
          await refresh();
        }
      } catch (err) {
        setMsg(String(err), 'err');
      }
    });

    searchEl.addEventListener('input', () => {
      clearTimeout(debounceTimer);
      debounceTimer = setTimeout(refresh, 180);
    });

    document.getElementById('refresh').addEventListener('click', refresh);
    document.getElementById('clear').addEventListener('click', async () => {
      if (!confirm('Delete all history entries? This cannot be undone.')) return;
      try {
        await invoke('clear_history');
        setMsg('History cleared.', 'ok');
        await refresh();
      } catch (err) {
        setMsg(String(err), 'err');
      }
    });

    async function boot() {
      for (let i = 0; i < 50; i++) {
        if (window.__TAURI__?.core) {
          await refresh();
          return;
        }
        await new Promise(r => setTimeout(r, 100));
      }
      setMsg('Could not connect to DictaFlow.', 'err');
    }

    boot();

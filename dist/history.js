(async () => {
  const searchEl = document.getElementById('search');
  const listEl = document.getElementById('list');
  const countEl = document.getElementById('count');
  const msgEl = document.getElementById('msg');
  const setMsg = (text, kind) => DF.setMsg(msgEl, text, kind);

  const tauri = await DF.tauri();
  if (!tauri) {
    setMsg('Could not connect to DictaFlow.', 'err');
    return;
  }
  const { invoke } = tauri.core;

  let debounceTimer = null;
  let entries = [];

  function formatTime(iso) {
    try {
      return new Date(iso).toLocaleString();
    } catch {
      return iso;
    }
  }

  function el(tag, className, text) {
    const node = document.createElement(tag);
    if (className) node.className = className;
    if (text !== undefined) node.textContent = text;
    return node;
  }

  function button(action, label, className) {
    const b = el('button', className, label);
    b.dataset.action = action;
    return b;
  }

  function renderEntry(e) {
    const row = el('div', 'entry');
    row.dataset.id = e.id;

    const head = el('div', 'entry-head');
    head.append(
      el('span', 'entry-time', formatTime(e.created_at)),
      el('span', 'entry-dur', `${Number(e.duration_secs ?? 0).toFixed(1)}s`),
    );
    const edit = el('textarea', 'entry-edit');
    edit.value = e.cleaned;
    row.append(head, edit);
    if (e.raw !== e.cleaned) row.append(el('div', 'entry-raw', e.raw));

    const actions = el('div', 'entry-actions');
    actions.append(
      button('save', 'Save'),
      button('copy-cleaned', 'Copy cleaned'),
      button('copy-raw', 'Copy raw'),
      button('delete', 'Delete', 'danger'),
    );
    row.append(actions);
    return row;
  }

  function updateCount() {
    countEl.textContent = `${entries.length} entr${entries.length === 1 ? 'y' : 'ies'}`;
    if (!entries.length) {
      listEl.replaceChildren(el('div', 'empty', 'No dictations yet. Hold your hotkey and speak.'));
    }
  }

  function render() {
    listEl.replaceChildren(...entries.map(renderEntry));
    updateCount();
  }

  async function refresh() {
    setMsg('');
    try {
      entries = await invoke('search_history', { query: searchEl.value.trim() });
      render();
    } catch (err) {
      setMsg(String(err), 'err');
    }
  }

  // Save/delete patch the affected row in place, so scroll position and unsaved
  // edits in other entries survive.
  listEl.addEventListener('click', async (ev) => {
    const btn = ev.target.closest('button[data-action]');
    const row = btn?.closest('.entry');
    if (!row) return;
    const id = row.dataset.id;
    const entry = entries.find((e) => e.id === id);
    if (!entry) return;

    try {
      switch (btn.dataset.action) {
        case 'save': {
          const textarea = row.querySelector('textarea.entry-edit');
          const text = textarea.value.trim();
          await invoke('update_history_entry', { id, cleaned: text });
          entry.cleaned = textarea.value = text;
          setMsg('Saved.', 'ok');
          break;
        }
        case 'copy-cleaned':
          await invoke('copy_text', { text: entry.cleaned });
          setMsg('Copied cleaned text.', 'ok');
          break;
        case 'copy-raw':
          await invoke('copy_text', { text: entry.raw });
          setMsg('Copied raw text.', 'ok');
          break;
        case 'delete':
          await invoke('delete_history_entry', { id });
          entries = entries.filter((e) => e.id !== id);
          row.remove();
          updateCount();
          setMsg('Deleted.', 'ok');
          break;
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
      entries = [];
      render();
      setMsg('History cleared.', 'ok');
    } catch (err) {
      setMsg(String(err), 'err');
    }
  });

  refresh();
})();

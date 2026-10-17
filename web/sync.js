// 設定の「サーバー」タブ。会社などの SyncLane サーバーに接続して、選んだ会話を送る(任意)。
// index.html の後に読み込む。t() / I18N / applyLang() / invoke / $ は index.html のものを使う。
(() => {
  Object.assign(I18N.ja, {
    navServer: 'サーバー',
    srvIntro: '会社などが用意した SyncLane サーバーに、選んだ会話を送ります。接続しなければ、何も送りません。接続しても、初期状態では何も送りません。',
    srvUrl: 'サーバーのアドレス',
    srvUrlPlaceholder: 'https://sync.example.com',
    srvConnect: '接続する',
    srvCodeTitle: 'ブラウザで、次のコードを入力してください',
    srvCodeOpened: 'ブラウザを開きました。開かないときは、次のアドレスを自分で開いてください。',
    srvCodeNotOpened: 'ブラウザを開けませんでした。次のアドレスを自分で開いてください。',
    srvWaiting: '承認を待っています…',
    srvCancel: '中止',
    srvConnected: (server, user) => `${server} に接続中（${user}）`,
    srvMode: '送る会話',
    srvModeManual: '選んだ会話だけ',
    srvModeManualNote: '下の一覧で、送る会話にチェックを入れます。',
    srvModeAuto: '除外した会話以外の全部',
    srvModeAutoNote: 'これから取り込む会話も、自動で送ります。送りたくない会話は、チェックを外します。',
    srvStatus: (sent, pending) => `送り済み ${sent} 件、待ち ${pending} 件`,
    srvLast: (at) => `最後に送った時刻: ${at}`,
    srvNever: 'まだ送っていません',
    srvRunning: '送信中…',
    srvSendNow: '今すぐ送る',
    srvDisconnect: '接続を解除',
    srvDisconnectConfirm: '接続を解除します。送り済みの会話は、サーバーに残ります。よければ、もう一度押してください。',
    srvDisconnectYes: '解除する',
    srvBlocked: (m) => `送信を止めています: ${m}`,
    srvBlockedHint: '解決するには、接続を解除して、もう一度つないでください。',
    srvVisibility: '新しく送る会話の公開範囲',
    srvVisDefault: 'サーバーの既定',
    srvVisPrivate: '自分だけ',
    srvVisOrg: 'サーバーの全員',
    srvList: '会話の一覧',
    srvSelectAll: 'すべて送る対象にする',
    srvMore: 'さらに表示',
    srvStateSend: '送る',
    srvStateExcluded: '除外',
    srvStateNone: '未選択',
    srvUnsent: (n) => `未送信 ${n}`,
    srvEvents: (n) => `${n}件`,
    srvWarn: '送るのは、会話の本文（あなたの発言と AI の応答）、題名、プロジェクトのパス、時刻です。パスワードやキーが会話に含まれていないか、確認してから選んでください。',
    srvConnectFailed: (m) => `接続できませんでした: ${m}`,
    srvError: (m) => `エラー: ${m}`,
    srvEmpty: '会話がまだありません。',
  });
  Object.assign(I18N.en, {
    navServer: 'Server',
    srvIntro: 'Send the conversations you choose to a SyncLane server (for example one your company runs). If you do not connect, nothing is sent. Even after connecting, nothing is sent at first.',
    srvUrl: 'Server address',
    srvUrlPlaceholder: 'https://sync.example.com',
    srvConnect: 'Connect',
    srvCodeTitle: 'Enter this code in your browser',
    srvCodeOpened: 'Your browser was opened. If it did not open, open this address yourself.',
    srvCodeNotOpened: 'Could not open your browser. Open this address yourself.',
    srvWaiting: 'Waiting for approval…',
    srvCancel: 'Cancel',
    srvConnected: (server, user) => `Connected to ${server} (${user})`,
    srvMode: 'What to send',
    srvModeManual: 'Only the conversations I pick',
    srvModeManualNote: 'Tick the conversations to send in the list below.',
    srvModeAuto: 'Everything except what I exclude',
    srvModeAutoNote: 'Conversations imported from now on are also sent automatically. Untick the ones you do not want to send.',
    srvStatus: (sent, pending) => `${sent} sent, ${pending} waiting`,
    srvLast: (at) => `Last sent: ${at}`,
    srvNever: 'Nothing sent yet',
    srvRunning: 'Sending…',
    srvSendNow: 'Send now',
    srvDisconnect: 'Disconnect',
    srvDisconnectConfirm: 'Conversations already sent stay on the server. Press again to disconnect.',
    srvDisconnectYes: 'Disconnect now',
    srvBlocked: (m) => `Sending is paused: ${m}`,
    srvBlockedHint: 'To fix it, disconnect and connect again.',
    srvVisibility: 'Visibility of conversations sent from now on',
    srvVisDefault: 'Server default',
    srvVisPrivate: 'Only me',
    srvVisOrg: 'Everyone on the server',
    srvList: 'Conversations',
    srvSelectAll: 'Select all to send',
    srvMore: 'Show more',
    srvStateSend: 'Send',
    srvStateExcluded: 'Excluded',
    srvStateNone: 'Not selected',
    srvUnsent: (n) => `${n} unsent`,
    srvEvents: (n) => `${n}`,
    srvWarn: 'What is sent: the text of the conversation (your messages and the AI replies), the title, the project path and the times. Check that no passwords or keys are in them before you pick.',
    srvConnectFailed: (m) => `Could not connect: ${m}`,
    srvError: (m) => `Error: ${m}`,
    srvEmpty: 'No conversations yet.',
  });

  const PAGE = 50;
  const root = $('sync-root');
  let overview = null;
  let rows = [];
  let visibility = '';
  let message = '';
  let urlDraft = '';
  let busy = false;

  const el = (tag, props = {}, ...children) => {
    const node = document.createElement(tag);
    for (const [key, value] of Object.entries(props)) {
      if (key === 'class') node.className = value;
      else if (key.startsWith('on')) node.addEventListener(key.slice(2), value);
      else if (value !== undefined && value !== false) node.setAttribute(key, value === true ? '' : value);
    }
    for (const child of children.flat()) {
      if (child !== null && child !== undefined && child !== false) node.append(child);
    }
    return node;
  };
  const note = (text, cls = '') => el('p', { class: `note ${cls}`.trim() }, text);
  const errorText = (e) => (typeof e === 'string' ? e : (e && e.message) || String(e));

  async function call(command, args) {
    try {
      return await invoke(command, args);
    } catch (e) {
      message = t('srvError', errorText(e));
      render();
      throw e;
    }
  }

  async function refresh(reloadRows = true) {
    if (!invoke || !root) return;
    try {
      overview = await invoke('sync_overview');
      if (overview.connection && reloadRows) {
        rows = await invoke('sync_sessions', { limit: Math.max(PAGE, rows.length), offset: 0 });
      } else if (!overview.connection) {
        rows = [];
      }
    } catch (e) {
      message = t('srvError', errorText(e));
    }
    render();
  }

  function renderConnect() {
    const connecting = overview.connecting;
    const children = [note(t('srvIntro'))];
    if (connecting) {
      children.push(
        el('div', { class: 'opt-title' }, t('srvCodeTitle')),
        el('p', { class: 'sync-code' }, connecting.user_code),
        note(t('srvCodeOpened')),
        el('textarea', { class: 'code', readonly: true, rows: '2', onclick: (e) => e.target.select() }, connecting.url),
        note(t('srvWaiting')),
        el('div', { class: 'opt-row' }, el('button', { type: 'button', onclick: async () => { await call('sync_connect_cancel'); message = ''; refresh(); } }, t('srvCancel'))),
      );
    } else {
      const input = el('input', { type: 'url', id: 'sync-url', placeholder: t('srvUrlPlaceholder'), value: urlDraft, autocomplete: 'off', spellcheck: 'false' });
      input.addEventListener('input', () => { urlDraft = input.value; });
      const button = el('button', { class: 'primary', type: 'button', disabled: busy }, t('srvConnect'));
      const connect = async () => {
        if (!urlDraft.trim() || busy) return;
        busy = true; message = ''; render();
        try {
          await invoke('sync_connect', { url: urlDraft.trim() });
        } catch (e) {
          message = t('srvConnectFailed', errorText(e));
        }
        busy = false;
        await refresh(false);
      };
      button.addEventListener('click', connect);
      input.addEventListener('keydown', (e) => { if (e.key === 'Enter') connect(); });
      children.push(el('div', { class: 'opt-title' }, t('srvUrl')), el('div', { class: 'opt-row' }, input, button));
    }
    return children;
  }

  function stateLabel(state) {
    return state === 'send' ? t('srvStateSend') : state === 'excluded' ? t('srvStateExcluded') : t('srvStateNone');
  }

  function renderSessions() {
    const auto = overview.mode === 'auto';
    const list = el('div', { class: 'sync-list' });
    if (!rows.length) list.append(note(t('srvEmpty')));
    for (const row of rows) {
      const box = el('input', { type: 'checkbox', checked: row.state === 'send' });
      box.addEventListener('change', async () => {
        // 外したとき: 自動のモードでは「除外」、手動のモードでは「指定なし」
        const state = box.checked ? 'send' : (auto ? 'excluded' : null);
        await call('sync_select', { ids: [row.id], state, visibility: box.checked && visibility ? visibility : null });
        refresh();
      });
      const name = row.title || row.project_path || `#${row.id}`;
      list.append(el('label', { class: `sync-row ${row.state}` },
        box,
        el('span', { class: 'sync-name', title: name }, name),
        el('span', { class: 'sync-meta' }, `${(row.started_at || '').slice(0, 10)} · ${row.source_tool} · ${t('srvEvents', row.events)}${row.unsent_events && row.state === 'send' ? ` · ${t('srvUnsent', row.unsent_events)}` : ''}`),
        el('span', { class: 'sync-state' }, stateLabel(row.state)),
      ));
    }
    const more = rows.length >= PAGE && rows.length % PAGE === 0
      ? el('button', { type: 'button', onclick: async () => { rows = rows.concat(await call('sync_sessions', { limit: PAGE, offset: rows.length })); render(); } }, t('srvMore'))
      : null;
    return [list, more];
  }

  function radio(name, value, checked, label, noteText, onchange) {
    const input = el('input', { type: 'radio', name, value, checked });
    input.addEventListener('change', onchange);
    return el('label', { class: 'sync-radio' }, input, el('span', {}, el('strong', {}, label), el('span', { class: 'sync-radio-note' }, noteText)));
  }

  function renderConnected() {
    const c = overview.connection;
    const counts = overview.counts || {};
    const user = c.user_name || c.user_email || '';
    const last = counts.last_sent_at ? new Date(counts.last_sent_at).toLocaleString() : t('srvNever');
    const children = [
      el('p', { class: 'sync-connected' }, t('srvConnected', c.server, user)),
      el('div', { class: 'opt-title' }, t('srvMode')),
      el('div', { class: 'sync-radios' },
        radio('sync-mode', 'manual', overview.mode === 'manual', t('srvModeManual'), t('srvModeManualNote'), async () => { await call('sync_set_mode', { mode: 'manual' }); refresh(); }),
        radio('sync-mode', 'auto', overview.mode === 'auto', t('srvModeAuto'), t('srvModeAutoNote'), async () => { await call('sync_set_mode', { mode: 'auto' }); refresh(); }),
      ),
      note(`${t('srvStatus', counts.events_sent || 0, counts.events_pending || 0)} · ${t('srvLast', last)}`),
    ];
    if (overview.blocked) {
      children.push(note(t('srvBlocked', overview.blocked), 'warn'), note(t('srvBlockedHint')));
    } else if (overview.last_result?.result?.stop) {
      children.push(note(overview.last_result.result.stop, 'warn'));
    }
    const sendNow = el('button', { class: 'primary', type: 'button', disabled: overview.running }, overview.running ? t('srvRunning') : t('srvSendNow'));
    sendNow.addEventListener('click', async () => { await call('sync_now'); setTimeout(() => refresh(false), 600); });
    const disconnect = el('button', { type: 'button' }, t('srvDisconnect'));
    // WebView では confirm() が出ないので、画面の中で 2 度押しにする
    const disconnectNote = note(t('srvDisconnectConfirm'), 'warn');
    disconnectNote.hidden = true;
    let armed = false;
    disconnect.addEventListener('click', async () => {
      if (!armed) {
        armed = true;
        disconnectNote.hidden = false;
        disconnect.textContent = t('srvDisconnectYes');
        return;
      }
      await call('sync_disconnect'); message = ''; refresh();
    });
    children.push(el('div', { class: 'opt-row' }, sendNow, disconnect), disconnectNote);

    const vis = el('select', { id: 'sync-visibility' },
      el('option', { value: '' }, t('srvVisDefault')),
      el('option', { value: 'private' }, t('srvVisPrivate')),
      el('option', { value: 'org' }, t('srvVisOrg')));
    vis.value = visibility;
    vis.addEventListener('change', () => { visibility = vis.value; });
    const selectAll = el('button', { type: 'button' }, t('srvSelectAll'));
    selectAll.addEventListener('click', async () => { await call('sync_select_all', { visibility: visibility || null }); refresh(); });
    children.push(
      el('div', { class: 'opt-title' }, t('srvList')),
      el('div', { class: 'opt-row' }, el('label', { for: 'sync-visibility' }, t('srvVisibility')), vis),
      overview.mode === 'manual' ? el('div', { class: 'opt-row' }, selectAll) : null,
      renderSessions(),
      note(t('srvWarn'), 'warn'),
    );
    return children;
  }

  function render() {
    if (!root || !overview) return;
    const nodes = [...(overview.connection ? renderConnected() : renderConnect()), message ? note(message, 'warn') : null];
    root.replaceChildren(...nodes.flat().filter(Boolean)); // null を渡すと、文字の "null" が入ってしまう
  }

  // 言語を切り替えたときに、このタブも描き直す
  const baseApplyLang = applyLang;
  applyLang = function () { baseApplyLang(); render(); };

  // タブを開いたときに最新にする
  document.querySelectorAll('.settings-nav-item').forEach((btn) => {
    if (btn.dataset.tab === 'server') btn.addEventListener('click', () => refresh());
  });
  const listen = window.__TAURI__?.event?.listen;
  if (listen && !isSearchWindow) {
    listen('sync-updated', () => refresh());
    listen('sync-connected', (event) => {
      message = event.payload?.ok ? '' : t('srvConnectFailed', event.payload?.message || '');
      refresh();
    });
  }
  applyLang();
  refresh();
})();

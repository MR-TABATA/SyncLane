// 設定の「サーバー」タブ。会社などの SyncLane サーバーに接続して、選んだ会話を送る(任意)。
// index.html の後に読み込む。t() / I18N / applyLang() / invoke / $ は index.html のものを使う。
(() => {
  Object.assign(I18N.ja, {
    navServer: 'サーバー',
    srvIntro: '会社などが用意した SyncLane サーバーに、会話を送ります。接続しなければ、何も送りません。接続すると、除外したもの以外を、すべて送ります。',
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
    srvModeManualNote: '「会話を個別に指定する」で、送る会話にチェックを入れます。',
    srvModeAuto: 'すべて（除外したものを除く）',
    srvModeAutoNote: 'これから取り込む会話も、自動で送ります。送らないものは、下で指定します。',
    srvRules: '送らないもの',
    srvSince: 'この日より前に始めた会話は送らない',
    srvTools: 'ツール（チェックを外すと送らない）',
    srvProjects: 'プロジェクト（チェックを外すと送らない）',
    srvListDetail: '会話を個別に指定する',
    srvListNote: 'ルールの例外です。ルールで除外された会話も、チェックを入れれば送ります。',
    srvStateRule: 'ルールで除外',
    srvStatus: (sent, pending) => `送り済み ${sent} 件、待ち ${pending} 件`,
    srvLast: (at) => `最後に送った時刻: ${at}`,
    srvNever: 'まだ送っていません',
    srvRunning: '送信中…',
    srvSendNow: '今すぐ送る',
    srvAutoNote: '新しい会話は、1 分おきに自動で送ります。ボタンは、待たずに送りたいときだけ押します。',
    srvDisconnect: '接続を解除',
    srvDisconnectConfirm: '接続を解除します。送り済みの会話は、サーバーに残ります。よければ、もう一度押してください。',
    srvDisconnectYes: '解除する',
    srvBlocked: (m) => `送信を止めています: ${m}`,
    srvBlockedHint: '解決するには、接続を解除して、もう一度つないでください。',
    srvSelectAll: 'すべて送る対象にする',
    srvMore: 'さらに表示',
    srvStateSend: '送る',
    srvStateExcluded: '除外',
    srvStateNone: '未選択',
    srvUnsent: (n) => `未送信 ${n}`,
    srvEvents: (n) => `${n}件`,
    srvWarn: '送るのは、会話の本文（あなたの発言と AI の応答）、題名、プロジェクトのパス、時刻です。公開範囲は、サーバーの設定で決まります。',
    srvConnectFailed: (m) => `接続できませんでした: ${m}`,
    srvError: (m) => `エラー: ${m}`,
    srvEmpty: '会話がまだありません。',
  });
  Object.assign(I18N.en, {
    navServer: 'Server',
    srvIntro: 'Sends your conversations to a SyncLane server set up by your company. Nothing is sent unless you connect. Once connected, everything is sent except what you exclude.',
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
    srvModeManualNote: 'Tick the conversations to send under “Pick conversations one by one”.',
    srvModeAuto: 'Everything (except what I exclude)',
    srvModeAutoNote: 'Conversations imported from now on are also sent automatically. Choose what not to send below.',
    srvRules: 'What not to send',
    srvSince: 'Do not send conversations that started before',
    srvTools: 'Tools (untick to skip)',
    srvProjects: 'Projects (untick to skip)',
    srvListDetail: 'Pick conversations one by one',
    srvListNote: 'These are exceptions to the rules. A conversation excluded by a rule is sent if you tick it.',
    srvStateRule: 'Excluded by rule',
    srvStatus: (sent, pending) => `${sent} sent, ${pending} waiting`,
    srvLast: (at) => `Last sent: ${at}`,
    srvNever: 'Nothing sent yet',
    srvRunning: 'Sending…',
    srvSendNow: 'Send now',
    srvAutoNote: 'New conversations are sent automatically every minute. Press the button only if you do not want to wait.',
    srvDisconnect: 'Disconnect',
    srvDisconnectConfirm: 'Conversations already sent stay on the server. Press again to disconnect.',
    srvDisconnectYes: 'Disconnect now',
    srvBlocked: (m) => `Sending is paused: ${m}`,
    srvBlockedHint: 'To fix it, disconnect and connect again.',
    srvSelectAll: 'Select all to send',
    srvMore: 'Show more',
    srvStateSend: 'Send',
    srvStateExcluded: 'Excluded',
    srvStateNone: 'Not selected',
    srvUnsent: (n) => `${n} unsent`,
    srvEvents: (n) => `${n}`,
    srvWarn: 'What is sent: the text of the conversation (your messages and the AI replies), the title, the project path and the times. Who can see them is set on the server.',
    srvConnectFailed: (m) => `Could not connect: ${m}`,
    srvError: (m) => `Error: ${m}`,
    srvEmpty: 'No conversations yet.',
  });

  const PAGE = 50;
  const root = $('sync-root');
  let overview = null;
  let rows = [];
  let ruleOpts = null;
  let listOpen = false;
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
        ruleOpts = await invoke('sync_rules');
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
        el('div', { class: 'sync-code-row' },
          el('p', { class: 'sync-code' }, connecting.user_code),
          el('button', { type: 'button', onclick: async (e) => {
            const button = e.currentTarget;
            try { await navigator.clipboard.writeText(connecting.user_code); } catch (_) { return; }
            button.textContent = t('mcpCopied');
            setTimeout(() => { button.textContent = t('mcpCopy'); }, 1500);
          } }, t('mcpCopy')),
        ),
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
    return state === 'send' ? t('srvStateSend') : state === 'excluded' ? t('srvStateExcluded') : state === 'rule_excluded' ? t('srvStateRule') : t('srvStateNone');
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
        await call('sync_select', { ids: [row.id], state, visibility: null });
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

  async function saveRules(rules) {
    await call('sync_set_rules', { rules });
    refresh();
  }

  // 自動のモードで、送らないものをルールで決める(会話を 1 件ずつ選ばなくてよいように)
  function renderRules() {
    const rules = { exclude_projects: [], exclude_tools: [], since: null, ...(ruleOpts?.rules || {}) };
    const toggle = (list, value, on) => (on ? list.filter((v) => v !== value) : [...list, value]);
    const checks = (items, key) => el('div', { class: 'sync-list sync-rules-list' }, ...items.map(([value, count]) => {
      const box = el('input', { type: 'checkbox', checked: !rules[key].includes(value) });
      box.addEventListener('change', () => saveRules({ ...rules, [key]: toggle(rules[key], value, box.checked) }));
      return el('label', { class: 'sync-row send' },
        box,
        el('span', { class: 'sync-name', title: value }, value),
        el('span', { class: 'sync-state' }, t('srvEvents', count)),
      );
    }));
    const since = el('input', { type: 'date', value: rules.since || '' });
    since.addEventListener('change', () => saveRules({ ...rules, since: since.value || null }));
    return el('div', { class: 'sync-rules' },
      el('div', { class: 'opt-title' }, t('srvRules')),
      el('div', { class: 'opt-row' }, el('label', {}, t('srvSince')), since),
      ruleOpts?.tools?.length ? [el('div', { class: 'opt-title' }, t('srvTools')), checks(ruleOpts.tools, 'exclude_tools')] : null,
      ruleOpts?.projects?.length ? [el('div', { class: 'opt-title' }, t('srvProjects')), checks(ruleOpts.projects, 'exclude_projects')] : null,
    );
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
        radio('sync-mode', 'auto', overview.mode === 'auto', t('srvModeAuto'), t('srvModeAutoNote'), async () => { await call('sync_set_mode', { mode: 'auto' }); refresh(); }),
        radio('sync-mode', 'manual', overview.mode === 'manual', t('srvModeManual'), t('srvModeManualNote'), async () => { await call('sync_set_mode', { mode: 'manual' }); refresh(); }),
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
    children.push(el('div', { class: 'opt-row' }, sendNow, disconnect), note(t('srvAutoNote')), disconnectNote);

    const selectAll = el('button', { type: 'button' }, t('srvSelectAll'));
    selectAll.addEventListener('click', async () => { await call('sync_select_all', { visibility: null }); refresh(); });
    const details = el('details', { class: 'sync-details', open: listOpen },
      el('summary', {}, t('srvListDetail')),
      note(t('srvListNote')),
      overview.mode === 'manual' ? el('div', { class: 'opt-row' }, selectAll) : null,
      ...renderSessions(),
    );
    details.addEventListener('toggle', () => { listOpen = details.open; });
    children.push(
      overview.mode === 'auto' ? renderRules() : null,
      details,
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

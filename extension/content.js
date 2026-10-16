// AI チャット（Gemini・ChatGPT）のページを見て、会話が変わったら（回答が書き終わるのを待ってから）SyncLane アプリへ送る。
// 送り先は background.js 経由で 127.0.0.1 のアプリだけ。アプリが動いていなければ、あとで送り直す。
// 会話の画面なのに何も取れないときは、拡張のアイコンに「!」を出す（画面の作りが変わった合図。直し方は extension/README.md）。
(function () {
  const QUIET_MS = 2500;   // 画面の変化が止まってから送る（回答の書き出し中に何度も送らない）
  const RETRY_MS = 15000;  // アプリが起動していなかったとき、送り直す間隔
  const BROKEN_MS = 10000; // 会話の画面で、これだけ待っても何も取れなければ「壊れた」とみなす
  let lastSent = "";       // 最後に送れた内容（同じ内容は送らない）
  let pending = "";        // 送りたいが、まだ送れていない内容
  let timer = null;
  let busy = false;
  let brokenTimer = null;
  let status = "";         // background に最後に伝えた状態（"broken" か ""）

  function snapshot() {
    const conv = window.SyncLaneExtract.conversation(document, location);
    return conv ? JSON.stringify(conv) : "";
  }

  function tell(state) {
    if (state === status) return;
    status = state;
    try { chrome.runtime.sendMessage({ type: "status", state }); } catch (error) {}
  }

  // 会話の画面（URL に会話の ID がある）なのに、何も取れない状態が続いたら、知らせる
  function watchBroken(now) {
    if (now || !window.SyncLaneExtract.expectsConversation(location)) {
      clearTimeout(brokenTimer);
      brokenTimer = null;
      tell("");
      return;
    }
    if (brokenTimer) return;
    brokenTimer = setTimeout(() => {
      brokenTimer = null;
      if (!snapshot() && window.SyncLaneExtract.expectsConversation(location)) {
        console.warn("[SyncLane] この画面から会話を読み取れません。画面の作りが変わった可能性があります（extension/README.md）。");
        tell("broken");
      }
    }, BROKEN_MS);
  }

  function schedule() {
    clearTimeout(timer);
    timer = setTimeout(flush, QUIET_MS);
  }

  async function flush() {
    const now = snapshot();
    watchBroken(now);
    if (!now || now === lastSent) { pending = ""; return; }
    pending = now;
    if (busy) return;
    busy = true;
    try {
      const payload = { ...JSON.parse(now), captured_at: new Date().toISOString() };
      const reply = await chrome.runtime.sendMessage({ type: "ingest", payload });
      if (reply && reply.ok) { lastSent = now; pending = ""; }
    } catch (error) {
      // 拡張が更新された直後など。次の機会に送り直す。
    } finally {
      busy = false;
    }
  }

  new MutationObserver(schedule).observe(document.body, { childList: true, subtree: true, characterData: true });
  window.addEventListener("popstate", schedule);
  setInterval(() => { if (pending || snapshot() !== lastSent) flush(); }, RETRY_MS);
  schedule();
})();

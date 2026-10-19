// content.js から会話を受け取り、SyncLane アプリ（この Mac の 127.0.0.1）へ渡す。
// 拡張のアイコンのバッジ（タブごと）:  「!」= 画面から会話を読み取れない  「×」= SyncLane アプリが起動していない
const ENDPOINT = "http://127.0.0.1:47821/ingest";

const BADGES = {
  broken: { text: "!", color: "#d97706", title: chrome.i18n.getMessage("badgeBrokenTitle") },
  offline: { text: "×", color: "#6b7280", title: chrome.i18n.getMessage("badgeOfflineTitle") },
};

function badge(tabId, key) {
  if (tabId === undefined) return;
  const b = BADGES[key];
  chrome.action.setBadgeText({ tabId, text: b ? b.text : "" });
  if (b) chrome.action.setBadgeBackgroundColor({ tabId, color: b.color });
  chrome.action.setTitle({ tabId, title: b ? b.title : chrome.i18n.getMessage("extName") });
}

chrome.runtime.onMessage.addListener((message, sender, sendResponse) => {
  const tabId = sender.tab && sender.tab.id;
  if (message && message.type === "ingest") {
    fetch(ENDPOINT, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(message.payload),
    })
      .then((response) => response.json())
      .then((json) => { badge(tabId, json && json.ok ? "" : "offline"); sendResponse(json); })
      .catch(() => { badge(tabId, "offline"); sendResponse({ ok: false }); }); // アプリが起動していない
    return true; // 非同期で返事をする
  }
  if (message && message.type === "status") {
    badge(tabId, message.state === "broken" ? "broken" : "");
  }
});

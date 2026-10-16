// content.js から会話を受け取り、SyncLane アプリ（この Mac の 127.0.0.1）へ渡す。
const ENDPOINT = "http://127.0.0.1:47821/ingest";

chrome.runtime.onMessage.addListener((message, _sender, sendResponse) => {
  if (message && message.type === "ingest") {
    fetch(ENDPOINT, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(message.payload),
    })
      .then((response) => response.json())
      .then((json) => sendResponse(json))
      .catch(() => sendResponse({ ok: false })); // アプリが起動していない
    return true; // 非同期で返事をする
  }
});

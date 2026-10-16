// Gemini の画面（DOM）から、会話を取り出す。画面に触るのはここだけ。
// Gemini が画面の作りを変えたら、直すのはこのファイルだけでよい。
//   - あなたの発言: <user-query>（本文は .query-text-line の並び。無ければ読み上げ用の見出しを除いた文字）
//   - Gemini の回答: <message-content>
(function (root) {
  function userText(node) {
    const lines = [...node.querySelectorAll(".query-text-line")].map((e) => e.innerText.trim()).filter(Boolean);
    if (lines.length) return lines.join("\n");
    const clone = node.cloneNode(true);
    clone.querySelectorAll(".cdk-visually-hidden").forEach((e) => e.remove());
    return (clone.innerText || "").trim();
  }

  function assistantText(node) {
    return (node.innerText || "").trim();
  }

  // URL から会話の ID を取る。/app/<id>、/gem/<gem>/<id>、/share/<id>。ID がまだ無い画面（新しいチャット）は null。
  function conversationId(pathname) {
    const parts = pathname.split("/").filter(Boolean);
    const last = parts[parts.length - 1];
    if (!last || !/^[A-Za-z0-9_-]{4,}$/.test(last)) return null;
    if (!["app", "share", "gem"].includes(parts[0]) || parts.length < 2) return null;
    return parts[0] === "share" ? "share_" + last : last;
  }

  function conversation(doc, loc) {
    const id = conversationId(loc.pathname);
    if (!id) return null;
    const messages = [];
    doc.querySelectorAll("user-query, message-content").forEach((node) => {
      const isUser = node.tagName === "USER-QUERY";
      if (!isUser && node.closest("user-query")) return;
      const text = isUser ? userText(node) : assistantText(node);
      if (text) messages.push({ index: messages.length, role: isUser ? "user" : "assistant", text });
    });
    if (!messages.length) return null;
    const first = messages.find((m) => m.role === "user");
    const title = first ? first.text.replace(/\s+/g, " ").slice(0, 60) : null;
    return { source: "gemini_web", conversation_id: id, title, messages };
  }

  root.SyncLaneExtract = { conversation, conversationId };
})(typeof window !== "undefined" ? window : globalThis);

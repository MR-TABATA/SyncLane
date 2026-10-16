// AI チャットの画面（DOM）から、会話を取り出す。画面に触るのはここだけ。
// サイトが画面の作りを変えたら、直すのはこのファイルだけでよい。
//
//   Gemini  (gemini.google.com)
//     - あなたの発言: <user-query>（本文は .query-text-line の並び。無ければ読み上げ用の見出しを除いた文字）
//     - Gemini の回答: <message-content>
//   ChatGPT (chatgpt.com)
//     - 発言の枠は [data-chatgpt-search-unit-key]（…:user / …:assistant）。今見ているページ（active-page=true）だけ読む
(function (root) {
  // 要素の文字を読む。通常は innerText（見た目どおりの改行）。
  // 画面から遠い古い発言は、サイトが中身の描画を省く（content-visibility / visibility:hidden）ため、
  // innerText が空になる（textContent には文字がある）。そのときは、段落ごとに改行を入れて、自分で組み立てる。
  // （2026-10 の Gemini で、長い会話の古い答えが取り込めなかった原因）
  const BLOCK = /^(P|DIV|LI|UL|OL|H[1-6]|PRE|BLOCKQUOTE|TABLE|TR|HR|SECTION|ARTICLE)$/;
  function textOf(node) {
    const shown = (node.innerText || "").trim();
    if (shown) return shown;
    const parts = [];
    (function walk(n, inPre) {
      for (const c of n.childNodes) {
        if (c.nodeType === 3) parts.push(inPre ? c.nodeValue : c.nodeValue.replace(/\s+/g, " "));
        else if (c.nodeType === 1) {
          if (c.tagName === "BR") { parts.push("\n"); continue; }
          if (c.tagName === "STYLE" || c.tagName === "SCRIPT") continue;
          const block = BLOCK.test(c.tagName);
          if (block) parts.push("\n");
          walk(c, inPre || c.tagName === "PRE");
          if (block) parts.push("\n");
        }
      }
    })(node, false);
    return parts.join("").split("\n").map((l) => l.trim()).join("\n").replace(/\n{3,}/g, "\n\n").trim();
  }

  const lastSegmentOk = (s) => !!s && /^[A-Za-z0-9_-]{4,}$/.test(s);

  // ---- Gemini ----

  function geminiUserText(node) {
    const lines = [...node.querySelectorAll(".query-text-line")].map((e) => textOf(e)).filter(Boolean);
    if (lines.length) return lines.join("\n");
    const clone = node.cloneNode(true);
    clone.querySelectorAll(".cdk-visually-hidden").forEach((e) => e.remove());
    return textOf(clone);
  }

  // /app/<id>、/gem/<gem>/<id>、/share/<id>。ID がまだ無い画面（新しいチャット）は null。
  function geminiConversationId(pathname) {
    const parts = pathname.split("/").filter(Boolean);
    const last = parts[parts.length - 1];
    if (!lastSegmentOk(last)) return null;
    if (!["app", "share", "gem"].includes(parts[0]) || parts.length < 2) return null;
    return parts[0] === "share" ? "share_" + last : last;
  }

  function geminiConversation(doc, loc) {
    const id = geminiConversationId(loc.pathname);
    if (!id) return null;
    const messages = [];
    doc.querySelectorAll("user-query, message-content").forEach((node) => {
      const isUser = node.tagName === "USER-QUERY";
      if (!isUser && node.closest("user-query")) return;
      const text = isUser ? geminiUserText(node) : textOf(node);
      if (text) messages.push({ index: messages.length, role: isUser ? "user" : "assistant", text });
    });
    return finish("gemini_web", id, null, messages);
  }

  // ---- ChatGPT ----

  // /c/<id>、/g/<gpt>/c/<id>（GPTs）、/share/<id>。ID がまだ無い画面（新しいチャット）は null。
  function chatgptConversationId(pathname) {
    const parts = pathname.split("/").filter(Boolean);
    const at = parts.indexOf("c");
    if (at >= 0 && lastSegmentOk(parts[at + 1])) return parts[at + 1];
    if (parts[0] === "share" && lastSegmentOk(parts[1])) return "share_" + parts[1];
    return null;
  }

  // 本文を取る。回答の「ChatGPT の発言:」のような、読み上げ用の見出し（h4）は除く。
  function chatgptUnitText(unit) {
    const body = unit.querySelector("[data-chatgpt-selection-message-id]");
    if (body) return textOf(body);
    const clone = unit.cloneNode(true);
    clone.querySelectorAll("h4").forEach((e) => e.remove());
    return textOf(clone);
  }

  function chatgptConversation(doc, loc) {
    const id = chatgptConversationId(loc.pathname);
    if (!id) return null;
    // 画面の裏に、ほかの会話のページが残っている。今見ているページ（active-page=true）だけを読む。
    const scope = doc.querySelector('[data-app-shell-active-page="true"]') || doc;
    const messages = [];
    // 現在の画面: 発言ごとの枠 [data-chatgpt-search-unit-key="…:user" | "…:assistant"]
    //   発言の ID は data-chatgpt-search-message-ids、発言の時刻は、その往復の <time datetime>
    scope.querySelectorAll("[data-chatgpt-search-unit-key]").forEach((unit) => {
      const key = unit.getAttribute("data-chatgpt-search-unit-key") || "";
      const role = key.endsWith(":user") ? "user" : key.endsWith(":assistant") ? "assistant" : null;
      if (!role) return;
      const turn = unit.closest("[data-turn-key]");
      const mark = (turn || unit).querySelector("[data-chatgpt-selection-conversation-id]");
      if (mark && !loc.pathname.includes(mark.getAttribute("data-chatgpt-selection-conversation-id"))) return; // ほかの会話
      const text = chatgptUnitText(unit);
      if (!text) return;
      const message = { index: messages.length, role, text };
      const own = (unit.getAttribute("data-chatgpt-search-message-ids") || "").split(/[\s,]+/)[0];
      if (own) message.id = own;
      if (role === "user") {
        const time = turn && turn.querySelector("time[datetime]");
        if (time) message.at = time.getAttribute("datetime");
      }
      messages.push(message);
    });
    // 以前の画面: [data-message-author-role]。現在の画面で何も取れなかったときだけ試す。
    if (!messages.length) {
      scope.querySelectorAll("[data-message-author-role]").forEach((node) => {
        const role = node.getAttribute("data-message-author-role");
        if (role !== "user" && role !== "assistant") return; // system や tool は取らない
        const text = textOf(node);
        if (!text) return;
        const message = { index: messages.length, role, text };
        const own = node.getAttribute("data-message-id");
        if (own) message.id = own;
        messages.push(message);
      });
    }
    // 画面のタブの題名が、会話の題名になっている（まだ付く前の "ChatGPT" だけのときは使わない）
    const tabTitle = (doc.title || "").replace(/\s+/g, " ").trim();
    return finish("chatgpt_web", id, tabTitle && tabTitle !== "ChatGPT" ? tabTitle.slice(0, 80) : null, messages);
  }

  // ---- 共通 ----

  function finish(source, id, title, messages) {
    if (!messages.length) return null;
    if (!title) {
      const first = messages.find((m) => m.role === "user");
      title = first ? first.text.replace(/\s+/g, " ").slice(0, 60) : null;
    }
    return { source, conversation_id: id, title, messages };
  }

  function conversation(doc, loc) {
    if (loc.hostname === "gemini.google.com") return geminiConversation(doc, loc);
    if (loc.hostname === "chatgpt.com") return chatgptConversation(doc, loc);
    return null;
  }

  // この URL が「会話の画面」か（会話の ID があるか）。会話の画面なのに conversation() が null なら、
  // 画面の作りが変わった可能性が高い（content.js が、拡張のアイコンに「!」を出す）。
  function expectsConversation(loc) {
    if (loc.hostname === "gemini.google.com") return !!geminiConversationId(loc.pathname);
    if (loc.hostname === "chatgpt.com") return !!chatgptConversationId(loc.pathname);
    return false;
  }

  root.SyncLaneExtract = { conversation, expectsConversation, conversationId: geminiConversationId, chatgptConversationId };
})(typeof window !== "undefined" ? window : globalThis);

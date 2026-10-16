# SyncLane for AI chats(Chrome 拡張)

Gemini(gemini.google.com)と ChatGPT(chatgpt.com)の会話を、この Mac の SyncLane アプリ(`127.0.0.1:47821`)にだけ送ります。入れ方は、リポジトリの README を見てください。

| ファイル | 役目 |
|---|---|
| `extract.js` | **画面(DOM)から会話を取り出す。サイトの画面の作りに触るのは、ここだけ。** |
| `content.js` | 画面の変化を見て、回答が書き終わったら送る。取れないときは、アイコンに「!」を出す |
| `background.js` | アプリへ渡す。アイコンのバッジ(「!」「×」)を出す |
| `test/index.html` | `extract.js` の試験(ブラウザで開くだけ) |

## 拡張のアイコンのバッジ

| 表示 | 意味 | すること |
|---|---|---|
| **×** | SyncLane アプリが起動していない | アプリを起動する(起動すると、自動で送り直す) |
| **!** | 会話の画面なのに、何も読み取れない。**画面の作りが変わった可能性が高い** | 下の「壊れたとき」 |

## 壊れたとき(「!」が出た、または会話が取り込まれなくなった)

Gemini と ChatGPT は、予告なく画面の作りを変えます。壊れるのは、`extract.js` の「どの要素が発言か」という目印が合わなくなるためです。直すのは、ほぼ `extract.js` だけです。

### 1. 画面の構造を調べる

壊れたサイトの会話のタブで、DevTools(Cmd+Option+I)の Console に貼って実行します。結果はクリップボードに入ります。

**発言らしい要素の目印を探す(どの `data-*` 属性があるか)**
```js
copy(JSON.stringify((() => {
  const attrs = {};
  document.querySelectorAll('*').forEach(e => [...e.attributes].forEach(a => {
    if (a.name.startsWith('data-')) attrs[a.name] = (attrs[a.name] || 0) + 1;
  }));
  const tags = [...new Set([...document.querySelectorAll('*')].map(e => e.tagName.toLowerCase()).filter(t => t.includes('-')))];
  return { title: document.title, path: location.pathname, attrs, tags };
})(), null, 1))
```
件数が「会話の発言の数」(または、その倍数)になっている属性やタグが、目印の候補です。

**候補の要素の構造を見る(1 往復ぶん。葉の先頭 12 文字だけ出る)**
```js
copy(JSON.stringify((() => {
  const target = document.querySelector('ここに候補のセレクタ');   // 例: '[data-turn-key]'
  const out = [];
  const walk = (e, d) => {
    if (d > 8 || out.length > 70) return;
    const at = [...e.attributes].filter(a => a.name !== 'class' && a.name !== 'style').map(a => a.name + '=' + a.value.slice(0, 36));
    out.push('  '.repeat(d) + e.tagName.toLowerCase() + ' ' + at.join(' ') + (e.children.length ? '' : ' | ' + (e.textContent || '').trim().slice(0, 12)));
    [...e.children].forEach(c => walk(c, d + 1));
  };
  walk(target, 0);
  return out;
})(), null, 1))
```

見つけたいもの:
- あなたの発言と、AI の回答の**本文の要素**(回答の「○○の発言:」のような、読み上げ用の見出しは除く)
- 発言ごとの **ID**(あれば。無くても、アプリ側が文面から ID を作る)
- 発言の**時刻**(あれば。無ければ、取り込んだ時刻になる)
- 画面に、**ほかの会話のページが裏に残っていないか**(ChatGPT は、見えているページだけを読む必要がある)

### 2. `extract.js` を直す
そのサイトの関数(`geminiConversation` または `chatgptConversation`)の、セレクタを直します。

### 3. 試験の部品を、新しい画面の構造に合わせる
`test/index.html` の部品(`geminiTurn`、`chatgptTurn`)を、1 で調べた**新しい構造に書き換えます**。部品が古いままだと、試験は通るのに、実際の画面では取れません。

```sh
cd extension && python3 -m http.server 8765 --bind 127.0.0.1
# ブラウザで http://127.0.0.1:8765/test/ を開く → 先頭が「ALL OK」なら合格
```

### 4. 実際の画面で確かめる
1. `chrome://extensions` で、この拡張の再読み込みボタンを押す。
2. そのサイトのタブを再読み込みして、数秒待つ。
3. 「!」が消え、SyncLane に会話が出ることを確認する。

## 仕組みのメモ
- アプリ側(`src/ingest.rs`)は、拡張が**会話の全体を毎回送る**ことを前提に、重複せず・順番を保って保存する。同じ内容を何度送っても増えない。
- 発言の ID は、画面が持っていればそれ(ChatGPT)、無ければ文面のハッシュ(Gemini)。
- 画面の作りが変わっても、すでに保存した会話は、そのまま残る。
- 拡張が送るのは、`127.0.0.1` のアプリだけ。アプリは、`Origin` が拡張のものと、JSON だけを受ける。

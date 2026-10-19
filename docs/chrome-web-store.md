# Chrome ウェブストア 申請メモ / Chrome Web Store submission notes

提出物: `sh scripts/pack-extension.sh` が `target/SyncLane-chrome-web-store.zip` を作る(manifest.json が直下)。

## 掲載文 / Listing

**名前 / Name**: SyncLane for AI chats

**短い説明 / Summary (132 字以内)**
Sends your Gemini and ChatGPT (web) conversations only to the SyncLane app on your Mac. Nothing goes to any external server.

**詳しい説明 / Description (EN)**
SyncLane for AI chats is the companion extension for SyncLane, a free macOS app that saves your AI conversations on your own Mac and lets you search them later.

Gemini and ChatGPT keep their web conversations on their own servers, so there is no local file for SyncLane to read. This extension reads the conversation you have open in Chrome and hands it to the SyncLane app running on the same Mac.

- Works on gemini.google.com and chatgpt.com
- Sends only to the SyncLane app on this Mac (127.0.0.1). No external server, no analytics, no account
- Sends again automatically if the app was not running
- Shows "!" on the toolbar icon if the page layout changed and the conversation cannot be read, and "×" if the app is not running

Requires the SyncLane app (macOS): https://mrktools.app/en/synclane/

**詳しい説明 / Description (JA)**
SyncLane for AI chats は、AI との会話を自分の Mac に保存して検索できる無料の macOS アプリ「SyncLane」の連携用拡張です。

Gemini と ChatGPT の Web 版の会話は、各社のサーバーにあり、Mac にファイルがありません。この拡張が、Chrome で開いている会話を読み取り、同じ Mac で動いている SyncLane アプリに渡します。

- gemini.google.com と chatgpt.com で動きます
- 送り先は、この Mac の SyncLane アプリ(127.0.0.1)だけ。外部のサーバー、アクセス解析、アカウントはありません
- アプリが起動していなかったときは、自動で送り直します
- 画面の作りが変わって読み取れないときはアイコンに「!」、アプリが起動していないときは「×」を出します

SyncLane アプリ(macOS)が必要です: https://mrktools.app/ja/synclane/

**カテゴリ**: 仕事効率化 / Productivity
**言語**: English(既定)、日本語

## 審査フォームの回答 / Privacy practices tab

**単一の目的 / Single purpose**
Send the conversation open on gemini.google.com or chatgpt.com to the SyncLane app on the same computer, so the user can save and search it there.

**権限の理由 / Permission justification**
- Host permission `http://127.0.0.1:47821/*`: the SyncLane app listens only on this local address. The extension posts the conversation there. No other address is contacted.
- Content scripts on `https://gemini.google.com/*` and `https://chatgpt.com/*`: to read the conversation shown on the page. These two sites are the only ones supported.

**リモートコード / Remote code**: 使っていない(No)。拡張のコードはすべて zip に入っている。

**収集するデータ / Data usage**: 「ウェブサイトのコンテンツ」(会話の本文)を扱う。ただし、この Mac の SyncLane アプリにだけ送る。開発者のサーバーには送らず、第三者にも渡さない。販売・転用・与信目的の利用はしない。ストアの3つの申告(販売しない／目的外に使わない／与信に使わない)に、すべてチェックを入れる。

**プライバシーポリシーの URL**: `https://mrktools.app/en/synclane/extension-privacy/`(日本語は `/ja/…`)。ページは MrkTools の `src/pages/[locale]/synclane/extension-privacy.astro`。**サイトをデプロイしてから**フォームに入れる。文面の元は `docs/extension-privacy.md`。

## 出す前に残っていること

- スクリーンショット: 撮影済み(`images_movies/chrome-web-store/01-saved-on-your-mac.png`、`02-search.png`。1280×800、ignore 済み)。3 枚目(`03-mcp.png`、MCP の設定)も撮影済み(0.2.1 を `/Applications` に入れて撮った。パスにユーザー名は写らない)。
- プライバシーポリシーの公開: ページは作成済み(未コミット)。MrkTools をコミットしてデプロイすると公開される。
- 開発者登録(Chrome ウェブストアの開発者アカウント。登録料は公式ページで確認)。
- `manifest.json` の version を、提出のたびに上げる(ストアは同じ版を受け付けない)。

#!/bin/sh
# SyncLane のリリース: universal(Apple Silicon + Intel)をビルド → Developer ID で署名 → 公証 → 更新用 tar.gz と署名
# → .dmg → mrktools.app に置くファイル(update.json / latest.json)を作る。
#
# 使い方:  sh scripts/release.sh            (版は src-tauri/tauri.conf.json の version)
#   SITE_DIR=...      出力先(既定 ~/Git/MrkTools/public/downloads/synclane)
#   NOTARY_PROFILE=...  notarytool のキーチェーンプロファイル(既定 mreditor)
#   SIGN_IDENTITY=...   署名する Developer ID
# 秘密鍵は ~/.tauri/synclane.key と synclane.key.password(リポジトリには入れない)。
set -eu

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SITE_DIR="${SITE_DIR:-$HOME/Git/MrkTools/public/downloads/synclane}"
NOTARY_PROFILE="${NOTARY_PROFILE:-mreditor}"
# 同名の証明書が System とログインの両方にあり、名前だと曖昧になるので、ログイン側のハッシュで指す(MrEditor と同じ)
SIGN_IDENTITY="${SIGN_IDENTITY:-CB470780A956A25E8DAE448646A06235156EDC38}"
KEY="$HOME/.tauri/synclane.key"
KEY_PASS="$(cat "$HOME/.tauri/synclane.key.password")"
TAURI="npx --yes @tauri-apps/cli@2.12.3"
BASE_URL="https://mrktools.app/downloads/synclane"

VERSION="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["version"])' "$ROOT/src-tauri/tauri.conf.json")"
OUT="$ROOT/src-tauri/target/release-work"
APP_DIR="$ROOT/src-tauri/target/universal-apple-darwin/release/bundle/macos"
APP="$APP_DIR/SyncLane.app"
DMG_NAME="SyncLane_${VERSION}_universal.dmg"
TGZ_NAME="SyncLane_${VERSION}.app.tar.gz"

rm -rf "$OUT" && mkdir -p "$OUT" "$SITE_DIR"

echo "== ビルド(universal) v$VERSION"
cd "$ROOT/src-tauri"
APPLE_SIGNING_IDENTITY="$SIGN_IDENTITY" \
TAURI_SIGNING_PRIVATE_KEY="$(cat "$KEY")" TAURI_SIGNING_PRIVATE_KEY_PASSWORD="$KEY_PASS" \
  $TAURI build --target universal-apple-darwin --bundles app

echo "== 署名の確認"
codesign --verify --deep --strict "$APP"

echo "== アプリの公証"
ditto -c -k --keepParent "$APP" "$OUT/app.zip"
xcrun notarytool submit "$OUT/app.zip" --keychain-profile "$NOTARY_PROFILE" --wait
xcrun stapler staple "$APP"

echo "== 更新用の tar.gz に署名(公証済みの .app を固める)"
# COPYFILE_DISABLE=1: macOS の tar は、拡張属性を「._ で始まるファイル」として混ぜる。それが入っていると、
# アプリの自動更新が展開に失敗する(0.2.0 で起きた)ので、混ぜない。
COPYFILE_DISABLE=1 tar -czf "$OUT/$TGZ_NAME" -C "$APP_DIR" SyncLane.app
# 念のため、生のアーカイブを読んで、._ が無いことを確かめる(macOS の tar は ._ を一覧から隠すので、Python で読む)
python3 - "$OUT/$TGZ_NAME" <<'PY'
import sys, tarfile
names = tarfile.open(sys.argv[1]).getnames()
bad = [n for n in names if any(part.startswith("._") for part in n.split("/"))]
if bad:
    sys.exit("更新用の tar.gz に ._ のファイルが混ざっています: %s" % bad)
print("更新用の tar.gz: %d 項目、._ なし" % len(names))
PY
TAURI_SIGNING_PRIVATE_KEY="$(cat "$KEY")" TAURI_SIGNING_PRIVATE_KEY_PASSWORD="$KEY_PASS" \
  $TAURI signer sign "$OUT/$TGZ_NAME"

echo "== dmg"
STAGE="$OUT/stage"; mkdir -p "$STAGE"
ditto "$APP" "$STAGE/SyncLane.app"
ln -s /Applications "$STAGE/Applications"
hdiutil create -volname "SyncLane" -srcfolder "$STAGE" -ov -format UDZO "$OUT/$DMG_NAME" >/dev/null
codesign --sign "$SIGN_IDENTITY" --timestamp "$OUT/$DMG_NAME"
xcrun notarytool submit "$OUT/$DMG_NAME" --keychain-profile "$NOTARY_PROFILE" --wait
xcrun stapler staple "$OUT/$DMG_NAME"

echo "== 配置: $SITE_DIR"
cp "$OUT/$DMG_NAME" "$OUT/$TGZ_NAME" "$SITE_DIR/"
python3 - "$SITE_DIR" "$VERSION" "$DMG_NAME" "$TGZ_NAME" "$BASE_URL" "$OUT/$TGZ_NAME.sig" <<'PY'
import datetime, hashlib, json, os, sys
site, version, dmg, tgz, base, sigfile = sys.argv[1:]
now = datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
sig = open(sigfile).read().strip()
url = f"{base}/{tgz}"
update = {"version": version, "notes": f"SyncLane {version}", "pub_date": now,
          "platforms": {"darwin-aarch64": {"signature": sig, "url": url},
                        "darwin-x86_64": {"signature": sig, "url": url}}}
path = os.path.join(site, dmg)
latest = {"version": version, "tag": f"v{version}", "published": now,
          "files": [{"name": dmg, "size": os.path.getsize(path),
                     "sha256": hashlib.sha256(open(path, "rb").read()).hexdigest()}]}
json.dump(update, open(os.path.join(site, "update.json"), "w"), indent=2); open(os.path.join(site, "update.json"), "a").write("\n")
json.dump(latest, open(os.path.join(site, "latest.json"), "w"), indent=2); open(os.path.join(site, "latest.json"), "a").write("\n")
PY
rm -rf "$OUT"
echo "完了: $DMG_NAME"
echo "次: ~/Git/MrkTools で commit → デプロイ。GitHub の Releases にも $DMG_NAME を上げる。"

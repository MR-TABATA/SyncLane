#!/bin/sh
# Chrome 拡張(extension/)を、配布用の zip にする。展開すると SyncLane-extension/ という 1 つのフォルダになる。
# 試験(test/)と、直す人向けの手順書(README.md)は入れない。
# あわせて、Chrome ウェブストアに出す zip(フォルダ無し・manifest.json が直下)を target/ に作る。
#
# 使い方:  sh scripts/pack-extension.sh
#   SITE_DIR=...   出力先(既定 ~/Git/MrkTools/public/downloads/synclane)
# 出力: $SITE_DIR/SyncLane-extension.zip(版の入らない固定の名前。版は、中の manifest.json の version)
set -eu

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SITE_DIR="${SITE_DIR:-$HOME/Git/MrkTools/public/downloads/synclane}"
STAGE="$ROOT/target/pack-extension"   # target/ は ignore 済み。/tmp は使わない
NAME="SyncLane-extension"

VERSION="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["version"])' "$ROOT/extension/manifest.json")"

rm -rf "$STAGE" && mkdir -p "$STAGE/$NAME" "$SITE_DIR"
for f in manifest.json extract.js content.js background.js; do
  cp "$ROOT/extension/$f" "$STAGE/$NAME/$f"
done
cp -R "$ROOT/extension/_locales" "$STAGE/$NAME/_locales"
mkdir "$STAGE/$NAME/icons" && cp "$ROOT"/extension/icons/*.png "$STAGE/$NAME/icons/"   # icon.svg(元の絵)は入れない

rm -f "$SITE_DIR/$NAME.zip"
(cd "$STAGE" && zip -q -r -X "$SITE_DIR/$NAME.zip" "$NAME")
STORE_ZIP="$ROOT/target/SyncLane-chrome-web-store.zip"
rm -f "$STORE_ZIP"
(cd "$STAGE/$NAME" && zip -q -r -X "$STORE_ZIP" .)
rm -rf "$STAGE"

echo "ウェブストア用 → $STORE_ZIP"
echo "拡張 v$VERSION → $SITE_DIR/$NAME.zip"
unzip -l "$SITE_DIR/$NAME.zip" | tail -n +4 | sed '$d' | sed '$d'

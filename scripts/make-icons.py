#!/usr/bin/env python3
"""SyncLane のアイコンを、1 つの定義(下の glyph)から全部作る。

使い方:  python3 scripts/make-icons.py
出力:    art/icon.svg(アプリ用。macOS の余白と影つき)・art/synclane-icon.svg(線画)・art/synclane-icon-white.svg(白抜き)
         extension/icons/icon.svg と PNG(拡張用。タイル全面)・src-tauri/icons/*(PNG・.icns・.ico)
         MrkTools の public/assets/icons/synclane.svg(LP 用。あれば)
PNG・.icns・.ico は rsvg-convert・iconutil・magick が要る。
"""
import os, subprocess, shutil, tempfile

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
W, CORAL, FUCHSIA = "#FFFFFF", "#FF8A3D", "#C2269F"

# 中央の細いレーン(節 3 つ)に、吹き出しが左・右・左と交互に並ぶ。尾は突き出さず、レーン側の下の角だけを小さい丸みにする。
G, BW, BH, GAP, R = 8, 27, 18, 6, 6          # レーンとの隙間・吹き出しの幅/高さ・縦の間隔・角丸
def bubble(x, y, corner, fill):
    cr = 1.8
    r_br = cr if corner == "br" else R
    r_bl = cr if corner == "bl" else R
    return (f'<path d="M{x+R} {y} H{x+BW-R} a{R} {R} 0 0 1 {R} {R} V{y+BH-r_br} a{r_br} {r_br} 0 0 1 -{r_br} {r_br} '
            f'H{x+r_bl} a{r_bl} {r_bl} 0 0 1 -{r_bl} -{r_bl} V{y+R} a{R} {R} 0 0 1 {R} -{R}Z" fill="{fill}"/>')

def glyph(bubble_color):
    total = 3 * BH + 2 * GAP
    y0 = (100 - total) / 2
    ys = [y0 + i * (BH + GAP) for i in range(3)]
    s = f'<path d="M50 {y0:g} V{y0+total:g}" stroke="{CORAL}" stroke-width="3" stroke-linecap="round" fill="none"/>'
    for y in ys:
        s += f'<circle cx="50" cy="{y+BH/2:g}" r="3" fill="{W}" stroke="{CORAL}" stroke-width="2.2"/>'
    s += bubble(50 - G - BW, ys[0], "br", bubble_color) + bubble(50 + G, ys[1], "bl", CORAL) + bubble(50 - G - BW, ys[2], "br", bubble_color)
    return s

DEFS = ('<defs><linearGradient id="fuchsia" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#DA3DB9"/><stop offset="1" stop-color="#A01C88"/></linearGradient>'
        '<radialGradient id="gloss" cx="50%" cy="0%" r="80%"><stop offset="0" stop-color="#fff" stop-opacity=".14"/><stop offset="55%" stop-color="#fff" stop-opacity="0"/></radialGradient></defs>')
TILE = '<rect width="100" height="100" rx="23" fill="url(#fuchsia)"/><rect width="100" height="100" rx="23" fill="url(#gloss)"/>'

def write(path, text):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    open(path, "w", encoding="utf-8").write(text)

APP = f'''<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024" viewBox="0 0 100 100">
  <!-- SyncLane アプリアイコン(大サイズ用・.icns / README / LP のマスター)。scripts/make-icons.py が作る。直すときはそちらを直す。
       フクシアのタイル(SyncVey と同じ配色の仲間: コーラル #FF8A3D)に、吹き出しが左・右・左と交互に並び、中央を細いレーンが縦に通る。
       Sync(やり取り)と Lane(レーン=時間軸)が 1 つの形に入る。尾は突き出さず、レーン側の下の角だけ小さい丸みにした。
       旧案(円形の同期矢印)は、Replacicon と、重なる 2 つの吹き出しは Lotus Chat と、形が近かったので外した(2026-10-19)。
       **カンバスいっぱいに描かないこと。** macOS のアプリアイコンは 1024px のうち角丸本体が 824px(= 80.47%)で、周囲に余白と落ち影を持つ。 -->
  {DEFS}
  <defs><filter id="shadow" x="-20%" y="-20%" width="140%" height="140%"><feDropShadow dx="0" dy="1.6" stdDeviation="1.6" flood-color="#0B1020" flood-opacity="0.30"/></filter></defs>
  <g filter="url(#shadow)">
    <g transform="translate(9.765 9.765) scale(0.8047)">
      {TILE}
      {glyph(W)}
    </g>
  </g>
</svg>
'''
write(f"{ROOT}/art/icon.svg", APP)
LINE = '<svg width="100" height="100" viewBox="0 0 100 100" xmlns="http://www.w3.org/2000/svg" fill="none" stroke-linecap="round" stroke-linejoin="round">\n  <!-- {note} -->\n  {body}\n</svg>\n'
write(f"{ROOT}/art/synclane-icon.svg", LINE.format(note="SyncLane アイコン(線画・タイルなし。明るい背景用)。scripts/make-icons.py が作る。", body=glyph(FUCHSIA)))
write(f"{ROOT}/art/synclane-icon-white.svg", LINE.format(note="暗い背景用(白抜き)。scripts/make-icons.py が作る。", body=glyph(W)))
EXT = f'''<svg xmlns="http://www.w3.org/2000/svg" width="128" height="128" viewBox="0 0 100 100">
  <!-- 拡張のアイコン。scripts/make-icons.py が作る。アプリアイコン用の余白と影を除き、ツールバーでは小さく出るので、タイルを全面に使う。 -->
  {DEFS}
  {TILE}
  {glyph(W)}
</svg>
'''
write(f"{ROOT}/extension/icons/icon.svg", EXT)

def png(svg, size, out):
    subprocess.run(["rsvg-convert", "-w", str(size), "-h", str(size), svg, "-o", out], check=True)

for n in (16, 32, 48, 128):
    png(f"{ROOT}/extension/icons/icon.svg", n, f"{ROOT}/extension/icons/icon-{n}.png")

icons = f"{ROOT}/src-tauri/icons"
for name in sorted(os.listdir(icons)):
    if name.endswith(".png"):   # 今ある PNG と同じ大きさで作り直す
        size = int(subprocess.check_output(["magick", "identify", "-format", "%w", f"{icons}/{name}"]).decode())
        png(f"{ROOT}/art/icon.svg", size, f"{icons}/{name}")
with tempfile.TemporaryDirectory(dir=os.path.join(ROOT, "target") if os.path.isdir(os.path.join(ROOT, "target")) else None) as tmp:
    iconset = f"{tmp}/icon.iconset"; os.makedirs(iconset)
    for size, nm in [(16,"icon_16x16"),(32,"icon_16x16@2x"),(32,"icon_32x32"),(64,"icon_32x32@2x"),(128,"icon_128x128"),(256,"icon_128x128@2x"),(256,"icon_256x256"),(512,"icon_256x256@2x"),(512,"icon_512x512"),(1024,"icon_512x512@2x")]:
        png(f"{ROOT}/art/icon.svg", size, f"{iconset}/{nm}.png")
    subprocess.run(["iconutil", "-c", "icns", iconset, "-o", f"{icons}/icon.icns"], check=True)
    ico = []
    for n in (16, 24, 32, 48, 64, 256):
        png(f"{ROOT}/art/icon.svg", n, f"{tmp}/i{n}.png"); ico.append(f"{tmp}/i{n}.png")
    subprocess.run(["magick", *ico, f"{icons}/icon.ico"], check=True)

site = os.path.expanduser("~/Git/MrkTools/public/assets/icons/synclane.svg")
if os.path.isdir(os.path.dirname(site)):
    shutil.copyfile(f"{ROOT}/art/icon.svg", site)
    print("LP のアイコンも更新:", site)
print("done")

"""Writes Japanese sample PDFs for comparing PDF text extraction (spec 28.9 step 2a).

Usage: python benches/pdf-compare/make-samples.py <output directory>

The samples cover horizontal and vertical writing, tables, two columns, fonts embedded as
TrueType subsets and standard CJK fonts that are not embedded, and a page with no text.
"""

import os
import sys

from reportlab.lib.pagesizes import A4
from reportlab.pdfbase import pdfmetrics
from reportlab.pdfbase.cidfonts import UnicodeCIDFont
from reportlab.pdfbase.ttfonts import TTFont
from reportlab.pdfgen import canvas

FONTS = os.path.join(os.environ.get("WINDIR", r"C:\Windows"), "Fonts")
pdfmetrics.registerFont(TTFont("Gothic", os.path.join(FONTS, "msgothic.ttc"), subfontIndex=0))


def cid(vertical):
    """Registers the standard Mincho font. Its vertical form has the same name, so each
    document registers the form it uses."""
    pdfmetrics.registerFont(UnicodeCIDFont("HeiseiMin-W3", isVertical=vertical))

WIDTH, HEIGHT = A4

PROSE = [
    "第1章　設備の概要",
    "本書は、冷却水ポンプの点検と保守の手順を説明する。",
    "ポンプは毎月1回、運転音と振動を確認する。異常があれば運転を止める。",
    "定格流量は毎分120リットル、定格揚程は25メートルである。",
    "交換部品の型番はPX-200（シール）とPX-310（軸受）とする。",
    "第2章　点検の手順",
    "電源を切り、表示灯が消えたことを確かめてから作業を始める。",
    "Mixed text: Dunlin reads PDF files, 2026年11月15日 版。",
]


def lines(page, font, size, rows, x=60, top=HEIGHT - 80, leading=22):
    page.setFont(font, size)
    for index, row in enumerate(rows):
        page.drawString(x, top - index * leading, row)


def horizontal(path, font):
    cid(False)
    page = canvas.Canvas(path, pagesize=A4)
    lines(page, font, 12, PROSE[:5])
    page.showPage()
    lines(page, font, 12, PROSE[5:])
    page.showPage()
    page.save()


def vertical(path):
    cid(True)
    page = canvas.Canvas(path, pagesize=A4)
    columns = ["縦書きの文書を読む。", "列は右から左へ進む。", "保守記録は三年間保管する。", "点検日は毎月十五日とする。"]
    page.setFont("HeiseiMin-W3", 14)
    for index, column in enumerate(columns):
        page.drawString(WIDTH - 80 - index * 30, HEIGHT - 80, column)
    page.showPage()
    page.save()


def table(path):
    page = canvas.Canvas(path, pagesize=A4)
    rows = [["部品", "型番", "交換周期", "数量"], ["シール", "PX-200", "6か月", "2"], ["軸受", "PX-310", "2年", "1"],
            ["フィルタ", "FL-15", "3か月", "4"]]
    page.setFont("Gothic", 11)
    page.drawString(60, HEIGHT - 60, "表1　交換部品")
    left, top, widths, height = 60, HEIGHT - 80, [120, 120, 120, 80], 24
    for r, row in enumerate(rows):
        x = left
        for c, cell in enumerate(row):
            page.rect(x, top - (r + 1) * height, widths[c], height)
            page.drawString(x + 6, top - (r + 1) * height + 8, cell)
            x += widths[c]
    page.showPage()
    page.save()


def two_columns(path):
    page = canvas.Canvas(path, pagesize=A4)
    page.setFont("Gothic", 14)
    page.drawString(60, HEIGHT - 50, "保守だより　第12号")
    left = ["左の段：今月の点検では", "ポンプ3台に異常は", "見つからなかった。", "次回は12月に行う。"]
    right = ["右の段：新しい手順書を", "配布した。旧版は", "回収して廃棄する。", "質問は保守課まで。"]
    lines(page, "Gothic", 11, left, x=60, top=HEIGHT - 90)
    lines(page, "Gothic", 11, right, x=WIDTH / 2 + 20, top=HEIGHT - 90)
    page.showPage()
    page.save()


def blank_middle(path):
    page = canvas.Canvas(path, pagesize=A4)
    lines(page, "Gothic", 12, ["1ページ目の本文。"])
    page.showPage()
    page.showPage()
    lines(page, "Gothic", 12, ["3ページ目の本文。"])
    page.showPage()
    page.save()


def long(path, pages=100):
    page = canvas.Canvas(path, pagesize=A4)
    for number in range(pages):
        rows = [f"{number + 1}-{row + 1}　" + PROSE[(number + row) % len(PROSE)] for row in range(30)]
        lines(page, "Gothic", 10, rows, top=HEIGHT - 50, leading=18)
        page.showPage()
    page.save()


def main():
    out = sys.argv[1]
    os.makedirs(out, exist_ok=True)
    horizontal(os.path.join(out, "horizontal-embedded.pdf"), "Gothic")
    horizontal(os.path.join(out, "horizontal-cid.pdf"), "HeiseiMin-W3")
    vertical(os.path.join(out, "vertical-cid.pdf"))
    table(os.path.join(out, "table.pdf"))
    two_columns(os.path.join(out, "two-columns.pdf"))
    blank_middle(os.path.join(out, "blank-page.pdf"))
    long(os.path.join(out, "long-100-pages.pdf"))


if __name__ == "__main__":
    main()

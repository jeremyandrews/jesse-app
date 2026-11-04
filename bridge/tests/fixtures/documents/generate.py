"""Regenerate the document attachment fixtures (bridge/src/documents.rs tests).

    python3 -m venv /tmp/fx && /tmp/fx/bin/pip install openpyxl==3.1.5 python-pptx==1.0.2
    /tmp/fx/bin/python -I generate.py <this directory>

macOS only: the Word and RTF fixtures are written by /usr/bin/textutil. Each fixture holds
one distinctive phrase the tests look for in the extracted text.
"""
import io
import os
import re
import subprocess
import sys
import zipfile

import openpyxl
from pptx import Presentation
from pptx.util import Inches

out = sys.argv[1]


def path(name):
    return os.path.join(out, name)


# Word and RTF, from one HTML source, through the converter the bridge reads them with.
html = path("source.html")
with open(html, "w") as f:
    f.write("<html><body><h1>Quarterly memo</h1>"
            "<p>The launch date is the fourteenth of March.</p></body></html>")
for fmt in ("docx", "doc", "rtf"):
    subprocess.run(["/usr/bin/textutil", "-convert", fmt, html, "-output",
                    path(f"memo.{fmt}")], check=True)
os.remove(html)

# The text family.
with open(path("note.txt"), "w") as f:
    f.write("Plain text fixture: the locker code is 7731.\n")
with open(path("readme.md"), "w") as f:
    f.write("# Packing list\n\n- passport\n- `adapter` for the plug\n")
with open(path("cities.csv"), "w") as f:
    f.write('city,population\nLisbon,545000\n"Porto, PT",232000\n')
with open(path("cities.tsv"), "w") as f:
    f.write("city\tpopulation\nLisbon\t545000\nPorto\t232000\n")
with open(path("quote.json"), "w") as f:
    f.write('{"pair": "XAU-USD", "price": 2650.5}\n')

# Excel: two sheets, a formula whose CACHED value is what must be read, and a date.
wb = openpyxl.Workbook()
ws = wb.active
ws.title = "Budget"
ws.append(["Item", "Cost"])
ws.append(["Flights", 1200])
ws.append(["Hotel, central", 800])
ws["A4"] = "Total"
ws["B4"] = "=SUM(B2:B3)"
ws["A5"] = "Booked"
ws["B5"] = __import__("datetime").date(2026, 3, 14)
notes = wb.create_sheet("Notes")
notes["A1"] = "Remember the travel insurance."
buf = io.BytesIO()
wb.save(buf)
# openpyxl writes a formula with no cached value; Excel always writes one. Add it, so the
# fixture looks like a workbook Excel saved.
src = zipfile.ZipFile(io.BytesIO(buf.getvalue()))
with zipfile.ZipFile(path("budget.xlsx"), "w", zipfile.ZIP_DEFLATED) as dst:
    for item in src.infolist():
        data = src.read(item.filename)
        if item.filename == "xl/worksheets/sheet1.xml":
            data = re.sub(rb"<f>SUM\(B2:B3\)</f><v\s*/>|<f>SUM\(B2:B3\)</f><v></v>|<f>SUM\(B2:B3\)</f>",
                          b"<f>SUM(B2:B3)</f><v>2000</v>", data, count=1)
        dst.writestr(item, data)

# PowerPoint: two slides with speaker notes, REORDERED so the part names (slide1, slide2)
# disagree with the order the deck shows them in.
deck = Presentation()
for title, body, note in [
    ("Roadmap", "Ship the bridge first", "Mention the budget slide."),
    ("Budget", "Flights & hotel: 2000", "Ask about insurance."),
]:
    slide = deck.slides.add_slide(deck.slide_layouts[1])
    slide.shapes.title.text = title
    slide.placeholders[1].text = body
    slide.notes_slide.notes_text_frame.text = note
ids = deck.slides._sldIdLst
first, second = list(ids)
ids.remove(second)
ids.insert(0, second)
deck.save(path("deck.pptx"))

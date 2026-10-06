import shutil
import subprocess
import tempfile
import textwrap
from pathlib import Path

import pymupdf
from PIL import Image, ImageOps, ImageSequence
from reportlab.lib.pagesizes import A4, letter
from reportlab.pdfgen.canvas import Canvas

OFFICE = {".doc", ".docx", ".odt", ".rtf", ".xls", ".xlsx", ".ods", ".csv", ".ppt", ".pptx", ".odp"}
IMAGES = {".jpg", ".jpeg", ".png", ".webp", ".gif", ".tif", ".tiff", ".bmp"}
SUPPORTED = {".pdf", ".txt", ".md"} | OFFICE | IMAGES


class ConversionError(Exception):
    pass


def office_profile(path):
    registry = path / "user"
    registry.mkdir(parents=True)
    # Disable macros and remote document links in the isolated conversion profile.
    (registry / "registrymodifications.xcu").write_text("""<?xml version="1.0"?>
<oor:items xmlns:oor="http://openoffice.org/2001/registry">
<item oor:path="/org.openoffice.Office.Common/Security/Scripting">
<prop oor:name="MacroSecurityLevel" oor:op="fuse"><value>3</value></prop>
<prop oor:name="DisableMacrosExecution" oor:op="fuse"><value>true</value></prop></item>
<item oor:path="/org.openoffice.Office.Common/Security">
<prop oor:name="BlockUntrustedRefererLinks" oor:op="fuse"><value>true</value></prop></item>
<item oor:path="/org.openoffice.Office.Calc/Content/Update">
<prop oor:name="Link" oor:op="fuse"><value>2</value></prop></item>
</oor:items>""")


def convert(source, output, paper="Letter", max_pages=50):
    suffix = source.suffix.lower()
    if suffix not in SUPPORTED:
        raise ConversionError("This file type cannot be printed. Send a PDF, document, or image.")
    size = A4 if paper == "A4" else letter
    try:
        if suffix == ".pdf":
            shutil.copyfile(source, output)
        elif suffix in IMAGES:
            Image.MAX_IMAGE_PIXELS = 40_000_000
            with Image.open(source) as img:
                canvas = Canvas(str(output), pagesize=size)
                for n, frame in enumerate(ImageSequence.Iterator(img)):
                    if n >= max_pages:
                        raise ConversionError(f"This file exceeds the {max_pages}-page limit.")
                    frame = ImageOps.exif_transpose(frame).convert("RGB")
                    if frame.width * frame.height > 40_000_000:
                        raise ConversionError("This image is too large to process safely.")
                    from reportlab.lib.utils import ImageReader

                    canvas.drawImage(
                        ImageReader(frame),
                        36,
                        36,
                        size[0] - 72,
                        size[1] - 72,
                        preserveAspectRatio=True,
                        anchor="c",
                    )
                    canvas.showPage()
                    if suffix == ".gif":
                        break
                canvas.save()
        elif suffix in {".txt", ".md"}:
            content = source.read_text(encoding="utf-8-sig")
            canvas = Canvas(str(output), pagesize=size)
            canvas.setFont("Courier", 10)
            y, pages = size[1] - 48, 1
            for line in content.expandtabs(4).splitlines():
                for fragment in textwrap.wrap(line, width=85, replace_whitespace=False) or [""]:
                    if y < 48:
                        pages += 1
                        if pages > max_pages:
                            raise ConversionError(f"This file exceeds the {max_pages}-page limit.")
                        canvas.showPage()
                        canvas.setFont("Courier", 10)
                        y = size[1] - 48
                    canvas.drawString(42, y, fragment)
                    y -= 14
            canvas.save()
        else:
            executable = shutil.which("libreoffice") or shutil.which("soffice")
            if not executable:
                raise ConversionError("Document conversion is available in the Docker service.")
            with tempfile.TemporaryDirectory(prefix="paperboy-office-") as work:
                work = Path(work)
                profile = work / "profile"
                office_profile(profile)
                command = [
                    executable,
                    f"-env:UserInstallation={profile.as_uri()}",
                    "--headless",
                    "--nologo",
                    "--nodefault",
                    "--norestore",
                    "--convert-to",
                    "pdf",
                    "--outdir",
                    str(work),
                    str(source),
                ]
                # Production calls this inside the networkless converter container.
                result = subprocess.run(command, capture_output=True, timeout=90, check=False)
                converted = work / (source.stem + ".pdf")
                if result.returncode or not converted.exists():
                    raise ConversionError(
                        "This document could not be converted. Try exporting it as PDF."
                    )
                shutil.copyfile(converted, output)
        with pymupdf.open(output) as document:
            if document.needs_pass:
                raise ConversionError("This PDF is password protected. Send an unlocked copy.")
            pages = len(document)
            if pages < 1 or pages > max_pages:
                raise ConversionError(f"This file must contain 1–{max_pages} pages.")
            # Rebuild PDF content as rendered pages; discard scripts, links and embedded files.
            clean = pymupdf.open()
            for page in document:
                if page.rect.width > 14400 or page.rect.height > 14400:
                    raise ConversionError("This document has an unusually large page.")
                scale = min(150 / 72, 2400 / max(page.rect.width, page.rect.height))
                pix = page.get_pixmap(matrix=pymupdf.Matrix(scale, scale), alpha=False)
                target = clean.new_page(width=size[0], height=size[1])
                target.insert_image(
                    target.rect + (24, 24, -24, -24), pixmap=pix, keep_proportion=True
                )
            cleaned = output.with_name("sanitized.pdf")
            clean.save(cleaned, garbage=4, deflate=True)
            clean.close()
        cleaned.replace(output)
        return pages
    except ConversionError:
        raise
    except subprocess.TimeoutExpired:
        raise ConversionError("Conversion took too long. Try exporting the file as PDF.") from None
    except Exception:
        raise ConversionError(
            "This file could not be opened. Send an unlocked PDF or a different copy."
        ) from None

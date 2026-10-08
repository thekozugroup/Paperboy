"""Exercise the actual networkless Rust image. Test fixtures never reach a printer."""

import random
import subprocess
import tempfile
from pathlib import Path

import pymupdf
from PIL import Image


def main():
    with tempfile.TemporaryDirectory(prefix=".converter-qa-", dir=Path.cwd()) as directory:
        root = Path(directory)
        root.chmod(0o755)
        inputs, outputs = root / "inputs", root / "outputs"
        inputs.mkdir(mode=0o755)
        outputs.mkdir(mode=0o777)
        outputs.chmod(0o777)
        Image.new("RGB", (120, 80), "white").save(inputs / "image.png")
        Image.new("RGB", (120, 80), "white").save(
            inputs / "scan.tiff",
            save_all=True,
            append_images=[Image.new("RGB", (120, 80), "gray")],
        )
        Image.new("1", (121, 80), 1).save(inputs / "fax.tif")
        Image.new("I;16", (120, 80), 65535).save(inputs / "deep-gray.tif")
        Image.new("CMYK", (120, 80), (0, 0, 0, 0)).save(inputs / "cmyk.tif")
        (inputs / "notes.txt").write_text(
            "Paperboy\nUnicode text: café, résumé.\n", encoding="utf-8"
        )
        (inputs / "document.rtf").write_text(r"{\rtf1\ansi Paperboy test document.}")
        document = pymupdf.open()
        page = document.new_page()
        page.insert_text((60, 60), "Paperboy safe PDF")
        page.insert_link(
            {
                "kind": pymupdf.LINK_URI,
                "from": pymupdf.Rect(60, 60, 250, 80),
                "uri": "https://example.com",
            }
        )
        document.embfile_add("extra.txt", random.Random(0).randbytes(20_000))
        document.save(inputs / "active.pdf")
        # File size in bytes must not be parsed as a page dimension in points.
        assert (inputs / "active.pdf").stat().st_size > 14_400
        document.save(
            inputs / "locked.pdf",
            encryption=pymupdf.PDF_ENCRYPT_AES_256,
            user_pw="test",
            owner_pw="owner",
        )
        document.close()
        document = pymupdf.open()
        for _ in range(3):
            document.new_page()
        document.save(inputs / "too-many.pdf")
        document.close()
        document = pymupdf.open()
        document.new_page(width=14_401, height=792)
        document.save(inputs / "oversized-page.pdf")
        document.close()
        for source in sorted(inputs.iterdir()):
            output = outputs / (source.name + ".pdf")
            limit = 2 if source.name == "too-many.pdf" else 50
            command = [
                "docker",
                "run",
                "--rm",
                "--network",
                "none",
                "--read-only",
                "--memory",
                "768m",
                "--cpus",
                "1",
                "--pids-limit",
                "64",
                "--cap-drop",
                "ALL",
                "--security-opt",
                "no-new-privileges:true",
                "--tmpfs",
                "/tmp:size=512m,mode=1777",
                "--mount",
                f"type=bind,src={inputs},dst=/input,readonly",
                "--mount",
                f"type=bind,src={outputs},dst=/output",
                "paperboy-converter:local",
                "paperboy-tools",
                "convert",
                f"/input/{source.name}",
                f"/output/{output.name}",
                "Letter",
                str(limit),
            ]
            result = subprocess.run(command, capture_output=True, text=True, timeout=140)
            if source.name in {"locked.pdf", "too-many.pdf", "oversized-page.pdf"}:
                assert result.returncode != 0, source.name
                assert not output.exists(), source.name
                print(f"{source.name}: rejected")
                continue
            assert result.returncode == 0, (source.name, result.stderr)
            expected = 2 if source.suffix == ".tiff" else 1
            with pymupdf.open(output) as converted:
                assert converted.page_count == expected, source.name
                assert converted.embfile_count() == 0, source.name
                assert all(not page.get_links() for page in converted), source.name
                assert all(
                    abs(page.rect.width - 612) < 1 and abs(page.rect.height - 792) < 1
                    for page in converted
                ), source.name
            print(f"{source.name}: {expected} safe page(s)")
    print("Rust Docker converter: all ten checks passed.")


if __name__ == "__main__":
    main()

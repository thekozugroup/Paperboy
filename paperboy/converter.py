"""Runs in a separate, networkless container; receives files through a Unix socket."""

import tempfile
from pathlib import Path

from fastapi import FastAPI, HTTPException, Request
from fastapi.responses import Response
from starlette.concurrency import run_in_threadpool

from paperboy.conversion import SUPPORTED, ConversionError, convert

app = FastAPI(docs_url=None, redoc_url=None, openapi_url=None)


@app.post("/convert")
async def convert_request(
    request: Request, extension: str, paper: str = "Letter", max_pages: int = 50
):
    if extension not in SUPPORTED or paper not in {"Letter", "A4"} or not 1 <= max_pages <= 100:
        raise HTTPException(400, "Invalid conversion options.")
    with tempfile.TemporaryDirectory(prefix="paperboy-") as work:
        source, output = Path(work) / ("source" + extension), Path(work) / "print.pdf"
        length = 0
        with source.open("wb") as file:
            async for chunk in request.stream():
                length += len(chunk)
                if length > 25 * 1024 * 1024:
                    raise HTTPException(413, "The file exceeds the size limit.")
                file.write(chunk)
        try:
            pages = await run_in_threadpool(convert, source, output, paper, max_pages)
        except ConversionError as error:
            raise HTTPException(422, str(error)) from None
        return Response(
            output.read_bytes(),
            media_type="application/pdf",
            headers={"X-Paperboy-Pages": str(pages)},
        )

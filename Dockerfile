FROM python:3.12-slim-bookworm AS base
ENV PYTHONDONTWRITEBYTECODE=1 PYTHONUNBUFFERED=1
WORKDIR /app
COPY requirements.txt ./
RUN pip install --no-cache-dir -r requirements.txt \
    && groupadd --gid 1000 paperboy \
    && useradd --uid 1000 --gid paperboy --create-home paperboy \
    && mkdir -p /run/paperboy && chown paperboy:paperboy /run/paperboy

FROM base AS converter
RUN apt-get update && apt-get install -y --no-install-recommends \
    libreoffice-writer libreoffice-calc libreoffice-impress fonts-dejavu-core fonts-noto-core \
    && rm -rf /var/lib/apt/lists/*
COPY paperboy ./paperboy
USER paperboy
ENV HOME=/tmp
CMD ["uvicorn", "paperboy.converter:app", "--uds", "/run/paperboy/convert.sock", "--no-access-log"]

FROM base AS app
RUN apt-get update && apt-get install -y --no-install-recommends \
    cups cups-client cups-filters libcups2-dev gcc tini \
    && pip install --no-cache-dir pycups==2.0.4 \
    && apt-get purge -y gcc libcups2-dev && apt-get autoremove -y \
    && rm -rf /var/lib/apt/lists/* \
    && usermod -aG lp,lpadmin paperboy
COPY paperboy ./paperboy
COPY web ./web
COPY docker/cupsd.conf /etc/cups/cupsd.conf
COPY docker/entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh
ENV PAPERBOY_DATA_DIR=/data PAPERBOY_CONVERTER_SOCKET=/run/paperboy/convert.sock \
    CUPS_SERVER=/run/cups/cups.sock
EXPOSE 8025
HEALTHCHECK --interval=30s --timeout=5s --start-period=20s CMD python -c "import urllib.request; urllib.request.urlopen('http://127.0.0.1:8025/api/health', timeout=3)"
ENTRYPOINT ["/usr/bin/tini", "--", "/entrypoint.sh"]

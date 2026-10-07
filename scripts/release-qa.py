"""Verify the actual registry anonymously, never from a cached/login-assisted pull."""

import json
import os
import urllib.request

version = os.environ["VERSION"]
for image in ("paperboy", "paperboy-converter"):
    repository = f"thekozugroup/{image}"
    with urllib.request.urlopen(
        f"https://ghcr.io/token?service=ghcr.io&scope=repository:{repository}:pull",
        timeout=30,
    ) as response:
        token = json.load(response)["token"]
    request = urllib.request.Request(
        f"https://ghcr.io/v2/{repository}/manifests/{version}",
        headers={
            "Authorization": f"Bearer {token}",
            "Accept": "application/vnd.oci.image.index.v1+json, application/vnd.docker.distribution.manifest.list.v2+json",
        },
    )
    with urllib.request.urlopen(request, timeout=30) as response:
        manifest = json.load(response)
    platforms = {
        (item["platform"]["os"], item["platform"]["architecture"]) for item in manifest["manifests"]
    }
    assert {("linux", "amd64"), ("linux", "arm64")} <= platforms, platforms
    print(f"{image}:{version}: anonymous AMD64/ARM64 access verified")

#!/usr/bin/env python3
from __future__ import annotations

import argparse
import gzip
import io
import tarfile
from pathlib import Path


def add_file(archive: tarfile.TarFile, source: Path, name: str, mode: int, mtime: int) -> None:
    data = source.read_bytes()
    info = tarfile.TarInfo(name)
    info.size = len(data)
    info.mode = mode
    info.mtime = mtime
    info.uid = 0
    info.gid = 0
    info.uname = "root"
    info.gname = "root"
    archive.addfile(info, io.BytesIO(data))


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--target", required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--epoch", type=int, required=True)
    args = parser.parse_args()

    root = f"agentic-footprint-v{args.version}-{args.target}"
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with args.output.open("wb") as raw:
        with gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=args.epoch) as compressed:
            with tarfile.open(fileobj=compressed, mode="w", format=tarfile.PAX_FORMAT) as archive:
                add_file(archive, args.binary, f"{root}/af", 0o755, args.epoch)
                add_file(archive, Path("install.sh"), f"{root}/install.sh", 0o755, args.epoch)
                add_file(archive, Path("LICENSE"), f"{root}/LICENSE", 0o644, args.epoch)
                add_file(archive, Path("README.md"), f"{root}/README.md", 0o644, args.epoch)


if __name__ == "__main__":
    main()

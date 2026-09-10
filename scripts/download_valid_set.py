#!/usr/bin/env python3
"""扩展验证集下载脚本：从 HuggingFace 公开二次元数据集下载分层样本。

分层（每层 6 张，共 30 张「序列首帧」，用于 CRF 压缩质量/码率标定）：
- illustration  赛璐璐/厚涂插画   Jannchie/illustration
- lineart       二次元线稿        ityizNola/Anime-LineArt-Dataset
- manga         黑白漫画          Chan-Y/Manga-Drawings
- pixel_sprite  像素画/精灵(含透明) KhalfounMehdi/gametilenet-sprites-unannotated
- wallpaper     高分辨率壁纸       puruchinera/anime_wallpapers

输出：E:\\CRF\\test\\png-valid\\<layer>\\<NN>.<ext>
依赖：httpx（已装）。默认走代理 127.0.0.1:7890，可用 HTTP_PROXY/HTTPS_PROXY 覆盖。

用法：
    python scripts/download_valid_set.py            # 下载全部分层
    python scripts/download_valid_set.py lineart    # 仅下载指定分层
"""
import os
import pathlib
import sys

os.environ.setdefault("HTTP_PROXY", "http://127.0.0.1:7890")
os.environ.setdefault("HTTPS_PROXY", "http://127.0.0.1:7890")

import httpx  # noqa: E402

BASE = pathlib.Path(r"E:\CRF\test\png-valid")
API = "https://huggingface.co/api/datasets"
RESOLVE = "https://huggingface.co/datasets"
EXTS = (".png", ".jpg", ".jpeg", ".webp")

# 分层 -> (HF repo, 文件列表子路径)
LAYERS = {
    "illustration": ("Jannchie/illustration", "images"),
    "lineart": ("ityizNola/Anime-LineArt-Dataset", ""),
    "manga": ("Chan-Y/Manga-Drawings", "images"),
    "pixel_sprite": ("KhalfounMehdi/gametilenet-sprites-unannotated", ""),
    "wallpaper": ("puruchinera/anime_wallpapers", ""),
}
PER_LAYER = 6


def list_images(repo: str, path: str) -> list[str]:
    url = f"{API}/{repo}/tree/main" + (f"/{path}" if path else "")
    r = httpx.get(url, params={"recursive": "true"}, timeout=90)
    r.raise_for_status()
    return [
        x["path"]
        for x in r.json()
        if x.get("type") == "file" and x["path"].lower().endswith(EXTS)
    ]


def download(repo: str, rfile: str, dest: pathlib.Path) -> int:
    url = f"{RESOLVE}/{repo}/resolve/main/{rfile}"
    r = httpx.get(url, follow_redirects=True, timeout=240)
    r.raise_for_status()
    dest.parent.mkdir(parents=True, exist_ok=True)
    dest.write_bytes(r.content)
    return len(r.content)


def main() -> None:
    only = sys.argv[1:] if len(sys.argv) > 1 else list(LAYERS)
    total = 0
    for layer in only:
        if layer not in LAYERS:
            print(f"未知分层: {layer}（可选: {', '.join(LAYERS)}）")
            continue
        repo, path = LAYERS[layer]
        print(f"=== {layer}: {repo} ===", flush=True)
        try:
            files = list_images(repo, path)
        except Exception as e:  # noqa: BLE001
            print(f"  list ERR: {e}")
            continue
        print(f"  {len(files)} images available", flush=True)
        for i, rfile in enumerate(files[:PER_LAYER]):
            ext = pathlib.Path(rfile).suffix.lower()
            dest = BASE / layer / f"{i:02d}{ext}"
            try:
                n = download(repo, rfile, dest)
                print(f"  [{i}] {rfile} -> {dest.name} ({n} B)", flush=True)
                total += 1
            except Exception as e:  # noqa: BLE001
                print(f"  [{i}] {rfile} ERR: {e}", flush=True)
    print(f"\nTotal downloaded: {total}")


if __name__ == "__main__":
    main()

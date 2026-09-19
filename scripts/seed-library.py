#!/usr/bin/env python3
"""T06 曲库造数：向 SQLite 写入可复现的 1 万/10 万条曲目元数据。

只用于压测检索端点的执行计划与翻页语义，不参与运行时。数据种子固定，
因此同一 `--count`/`--seed` 两次造出的库必须逐字节同序（可用 --verify-hash 自检）。

    python scripts/seed-library.py --db /tmp/litebeat.db --count 10000
    python scripts/seed-library.py --db /tmp/litebeat.db --count 100000 --clean

排序键规范化与 server/src/library/normalize.rs 同规则（NFKC → 压空白 → 小写）；
该 Rust 函数才是入库时的权威实现，这里只为造数在脚本侧复刻一份。
"""

from __future__ import annotations

import argparse
import hashlib
import random
import sqlite3
import sys
import time
import unicodedata
from pathlib import Path

MIGRATIONS = Path(__file__).resolve().parent.parent / "server" / "migrations"
DEFAULT_SEED = 20260919

# 中英文混合标题池：故意留下大量重复，验证 (sort_title,id) 的稳定翻页。
CN_TITLES = [
    "晴天", "稻香", "双截棍", "青花瓷", "烟花易冷", "简单爱", "七里香", "龙卷风",
    "他说", "周杰伦的床边故事", "花心", "千里之外", "菊花台", "东风破", "夜曲",
    "不爱我就拉倒", "我是如此相信", "最伟大的作品", "Mojito", "等你下课",
]
EN_TITLES = [
    "Bohemian Rhapsody", "bohemian rhapsody", "YELLOW", "Yellow", "clockwork",
    "Can't Help Falling in Love", "Hotel California", "impressionant",
    "Viva la Vida", "speed of light", "The Scientist", "the scientist",
    "Perfect", "Shape of You", "Sorry", "hello", "Rolling in the Deep",
    "Someone Like You", "Set Fire to the Rain", "50% Off", "_hidden track",
    "他说\"周杰\"", "ＪＡＹ　Zhou", "Back in Black", "Smoke on the Water",
]
CN_ARTISTS = ["周杰伦", "林俊杰", "陈奕迅", "张学友", "王菲", "邓紫棋", "李荣浩", "薛之谦"]
EN_ARTISTS = ["Queen", "BEYOND", "Adele", "Adele ", "The Beatles", "Coldplay", "Ed Sheeran"]
ALBUMS = [
    "叶惠美", "十一月的萧邦", "范特西", "跨时代", "最伟大的作品",
    "A Night at the Opera", "X&Y", "21", "÷", "美利坚",
]


def normalize(value: str) -> str:
    """复刻 Rust 侧 normalize：NFKC、压空白、小写。"""
    return " ".join(unicodedata.normalize("NFKC", value).split()).lower()


def apply_migrations(conn: sqlite3.Connection) -> int:
    """按文件名前缀顺序补齐迁移，user_version 语义与 Rust migrate.rs 一致。"""
    current = conn.execute("PRAGMA user_version").fetchone()[0]
    for path in sorted(MIGRATIONS.glob("*.sql")):
        version = int(path.name.split("_", 1)[0])
        if version <= current:
            continue
        conn.executescript(path.read_text(encoding="utf-8"))
        conn.execute(f"PRAGMA user_version = {version}")
        current = version
    conn.commit()
    return current


def build_rows(count: int, seed: int) -> tuple[list[tuple], dict[tuple[str, str], int]]:
    """生成 count 行 tracks；albums 由标题池组合而来，重复即复用同一专辑。"""
    rng = random.Random(seed)
    pool = CN_TITLES + EN_TITLES
    albums: dict[tuple[str, str], int] = {}
    rows: list[tuple] = []
    for index in range(count):
        # 70% 从标题池取（制造大量重名），30% 带序号（保证长尾唯一且可分页验证）。
        if rng.random() < 0.7:
            title = rng.choice(pool)
        else:
            base = rng.choice(pool)
            title = f"{base} ({index % 251} 现场版)"
        artist = rng.choice(CN_ARTISTS + EN_ARTISTS)
        album_title = rng.choice(ALBUMS)
        album_key = (normalize(album_title), normalize(artist))
        album_id = albums.setdefault(album_key, len(albums) + 1)
        rows.append(
            (
                f"seed/{index % 997:03d}/{index:07d}.flac",
                title,
                artist,
                album_title,
                normalize(title),
                normalize(artist),
                normalize(album_title),
                rng.choice([1, 1, 1, 2]),          # disc_no
                rng.randint(1, 14),                # track_no
                rng.randint(60_000, 480_000),      # duration_ms
                rng.choice(["flac", "opus", "mp3"]),
                1 if rng.random() < 0.985 else 0,  # available
                1_700_000_000 + index,             # mtime_ns：保证 recent 序稳定
            )
        )
    return rows, albums


def write(conn: sqlite3.Connection, count: int, root_path: str, seed: int) -> None:
    cur = conn.cursor()
    cur.execute(
        "INSERT INTO library_roots(name, canonical_path, enabled) VALUES (?,?,1)",
        ("seed-library", root_path),
    )
    root_id = cur.lastrowid
    rows, albums = build_rows(count, seed)

    cur.executemany(
        "INSERT INTO albums(root_id, directory_key, title, album_artist, sort_title, id)"
        " VALUES (?,?,?,?,?,?)",
        [
            (root_id, f"seed-album-{slot:03d}", title, artist, normalize(title), slot)
            for ((title, artist), slot) in albums.items()
        ],
    )
    cur.executemany(
        "INSERT INTO tracks(root_id, relative_path, title, artist, album_id, album_title,"
        " sort_title, sort_artist, sort_album, disc_no, track_no, duration_ms, codec, mime,"
        " size_bytes, available, mtime_ns)"
        " VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        [
            (
                root_id,
                rel,
                title,
                artist,
                albums[(normalize(album_title), normalize(artist))],
                album_title,
                sort_title,
                sort_artist,
                sort_album,
                disc,
                track,
                duration,
                codec,
                f"audio/{codec}",
                duration * 160,
                available,
                mtime,
            )
            for (
                rel, title, artist, album_title, sort_title, sort_artist, sort_album,
                disc, track, duration, codec, available, mtime,
            ) in rows
        ],
    )
    conn.commit()


def table_hash(conn: sqlite3.Connection) -> str:
    """对排序后的曲目主键与排序键取摘要，用于证明造数可复现。"""
    digest = hashlib.sha256()
    for row in conn.execute(
        "SELECT id, sort_title, sort_artist, sort_album, album_id, disc_no, track_no,"
        " duration_ms, available, mtime_ns FROM tracks ORDER BY id"
    ):
        digest.update(repr(row).encode())
    return digest.hexdigest()[:16]


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--db", required=True, type=Path, help="SQLite 文件路径")
    parser.add_argument("--count", type=int, default=10_000, help="写入曲目行数")
    parser.add_argument("--seed", type=int, default=DEFAULT_SEED, help="固定数据种子")
    parser.add_argument("--root", default="/seed/music", help="媒体根目录标识")
    parser.add_argument("--clean", action="store_true", help="清空 tracks/albums 后重造")
    args = parser.parse_args(argv)
    # Windows 控制台默认 GBK，中文摘要会乱码；统一按 UTF-8 输出。
    for stream in (sys.stdout, sys.stderr):
        stream.reconfigure(encoding="utf-8", errors="replace")

    args.db.parent.mkdir(parents=True, exist_ok=True)
    conn = sqlite3.connect(args.db)
    conn.execute("PRAGMA foreign_keys = ON")
    version = apply_migrations(conn)

    existing = conn.execute("SELECT COUNT(*) FROM tracks").fetchone()[0]
    if existing and not args.clean:
        print(f"库中已有 {existing} 行曲目；加 --clean 覆盖，否则混入数据会干扰计划核对", file=sys.stderr)
        return 2
    if args.clean:
        # 触发器会把 FTS 一起清掉，删顺序反过来即可。
        conn.execute("DELETE FROM tracks")
        conn.execute("DELETE FROM albums")
        conn.execute("DELETE FROM library_roots")
        conn.commit()

    started = time.perf_counter()
    write(conn, args.count, args.root, args.seed)
    elapsed = time.perf_counter() - started

    tracks = conn.execute("SELECT COUNT(*) FROM tracks").fetchone()[0]
    distinct = conn.execute("SELECT COUNT(DISTINCT sort_title) FROM tracks").fetchone()[0]
    album_rows = conn.execute("SELECT COUNT(*) FROM albums").fetchone()[0]
    fts = conn.execute("SELECT COUNT(*) FROM tracks_fts").fetchone()[0]
    print(
        f"schema v{version} seed={args.seed} 写入 tracks={tracks} albums={album_rows} "
        f"fts={fts} 去重标题={distinct} 耗时={elapsed:.2f}s 摘要={table_hash(conn)}"
    )
    conn.close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))

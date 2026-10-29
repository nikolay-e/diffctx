"""Deterministic shards that keep one repository in one shard.

A shard clones only the repositories it owns, which is what lets a free hosted
runner (a few GB of disk) hold a benchmark whose repositories total much more.
Repositories go largest-first onto the lightest shard, ties broken by name, so
the same input always yields the same split and a rerun of one shard is valid.
"""

from __future__ import annotations

from collections import Counter
from collections.abc import Callable, Sequence
from typing import TypeVar

T = TypeVar("T")


def assign_repos(repo_counts: Counter[str], shards: int) -> dict[str, int]:
    if shards < 1:
        raise ValueError(f"shards must be >= 1, got {shards}")
    load = [0] * shards
    owner: dict[str, int] = {}
    for repo, count in sorted(repo_counts.items(), key=lambda kv: (-kv[1], kv[0])):
        lightest = min(range(shards), key=lambda i: (load[i], i))
        owner[repo] = lightest
        load[lightest] += count
    return owner


def shard_items(items: Sequence[T], repo_of: Callable[[T], str], shards: int, index: int) -> list[T]:
    if not 0 <= index < shards:
        raise ValueError(f"shard index {index} outside 0..{shards - 1}")
    owner = assign_repos(Counter(repo_of(i) for i in items), shards)
    return [i for i in items if owner[repo_of(i)] == index]


def parse_shard(spec: str) -> tuple[int, int]:
    """`"2/5"` is shard index 2 of 5."""
    index_s, sep, total_s = spec.partition("/")
    if not sep:
        raise ValueError(f"shard must look like I/N, got {spec!r}")
    index, total = int(index_s), int(total_s)
    if total < 1 or not 0 <= index < total:
        raise ValueError(f"shard {spec!r} is out of range")
    return index, total

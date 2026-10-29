#!/usr/bin/env python3
"""盘点本机的 agent 宿主、skills 与 MCP，产出一份 Markdown 索引。

只读。不删、不改、不建除输出文件以外的任何东西。

为什么放在 tools/ 而不是做成 tern 的子命令：这是运维脚本，不是产品功能，
不该受 tern 的代码约束（--offline 构建、依赖白名单）连累。
标准库就够，不用装任何东西。

用法：
    python tools/inventory/scan.py                # 写到 tools/inventory/inventory.md
    python tools/inventory/scan.py -o out.md      # 指定输出
    python tools/inventory/scan.py --stdout       # 打到终端，不落盘
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import sys
from pathlib import Path

HOME = Path.home()

# 宿主 -> (skills 目录列表, mcp 配置文件列表)
# 目录不存在就跳过，所以可以放心把可能的路径都列上。
HOSTS: dict[str, dict[str, list[Path]]] = {
    "Claude Code": {
        "skills": [HOME / ".claude" / "skills"],
        "mcp": [HOME / ".claude" / "mcp.json"],
    },
    "Codex": {
        "skills": [HOME / ".codex" / "skills", HOME / ".cc-switch" / "skills"],
        "mcp": [HOME / ".codex" / "config.toml"],
    },
    "Cursor": {
        "skills": [HOME / ".cursor" / "skills", HOME / ".cursor" / "skills-cursor"],
        "mcp": [HOME / ".cursor" / "mcp.json"],
    },
    "WorkBuddy": {
        "skills": [HOME / ".workbuddy" / "skills"],
        "mcp": [HOME / ".workbuddy" / "mcp.json"],
    },
    "Windsurf": {"skills": [HOME / ".windsurf" / "skills"], "mcp": []},
    "Gemini": {"skills": [HOME / ".gemini" / "skills"], "mcp": []},
}

FRONTMATTER = re.compile(r"\A---\s*\n(.*?)\n---", re.S)


def parse_frontmatter(text: str) -> dict[str, str]:
    """取出 SKILL.md 头部 YAML 里的 name / description。

    不引 PyYAML：要的只是两个标量字段，而 description 经常是折行的块
    （`|` 或 `>-`），手写解析反而能精确控制"折行到哪结束"。
    """
    m = FRONTMATTER.match(text)
    if not m:
        return {}
    body = m.group(1)
    out: dict[str, str] = {}
    for key in ("name", "description"):
        # 键值在同一行，或是 `|` / `>` / `|-` 起头的块
        pat = re.compile(rf"^{key}:\s*(.*?)(?=^\S|\Z)", re.S | re.M)
        km = pat.search(body)
        if not km:
            continue
        raw = km.group(1)
        lines = [ln for ln in raw.splitlines()]
        if lines and lines[0].strip() in ("|", ">", "|-", ">-", "|+", ">+"):
            lines = lines[1:]
        out[key] = " ".join(" ".join(lines).split())
    return out


def sha8(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()[:8]


# 已知的"源 → 镜像"关系。
# cc-switch 把自己的 skills 库同步到 ~/.claude/skills，两边文件名一样、内容
# 通常逐字节相同。同步卡住时只有一边被改，就分叉了——而没有任何标记告诉用户
# 哪个是源头。这里把它写死，是为了让扫描结果能直接回答"该改哪个"。
# 值里第一个是源，其余是镜像。
MIRROR_SETS: dict[str, list[Path]] = {
    "cc-switch → claude": [HOME / ".cc-switch" / "skills", HOME / ".claude" / "skills"],
}


def dir_fingerprint(path: Path) -> str:
    """整个目录的内容指纹：所有文件的 相对路径 + 内容 排序后哈希。

    只看 SKILL.md 不够——`kill-ai-slop` 那次分叉里，`scripts/rules.zh.mjs`
    是新增文件，SKILL.md 的差异只是它的引用。整目录指纹才能判准。
    """
    h = hashlib.sha256()
    try:
        for f in sorted(path.rglob("*")):
            if f.is_file():
                h.update(str(f.relative_to(path)).replace("\\", "/").encode())
                h.update(b"\0")
                h.update(f.read_bytes())
    except OSError:
        return "<读取失败>"
    return h.hexdigest()[:10]


def scan_mirror_sets() -> list[dict]:
    """比对源与镜像，报告哪些 skill 分叉了。"""
    rows = []
    for label, paths in MIRROR_SETS.items():
        src = paths[0]
        if not src.is_dir():
            continue
        src_names = {d.name for d in src.iterdir() if d.is_dir()}
        for mirror in paths[1:]:
            if not mirror.is_dir():
                continue
            mirror_names = {d.name for d in mirror.iterdir() if d.is_dir()}
            for name in sorted(src_names & mirror_names):
                a = dir_fingerprint(src / name)
                b = dir_fingerprint(mirror / name)
                rows.append(
                    {
                        "set": label,
                        "name": name,
                        "same": a == b,
                        "src": str(src / name),
                        "mirror": str(mirror / name),
                        "src_fp": a,
                        "mirror_fp": b,
                    }
                )
    return rows


def scan_skill_dir(path: Path) -> list[dict]:
    """一个 skills 目录下的每个子目录算一个 skill。

    符号链接要跟着走（Codex 就是这么挂 `.cc-switch/skills` 下的条目的），
    但要在行里标出来——链接和真副本的处置方式完全不同：链接只改一处就同步了，
    真副本得两边都改。
    """
    rows = []
    if not path.is_dir():
        return rows
    for entry in sorted(path.iterdir()):
        if not entry.is_dir():
            continue
        link_target = ""
        if entry.is_symlink():
            try:
                link_target = str(entry.resolve())
            except OSError:
                link_target = "<断链>"
        skill_md = entry / "SKILL.md"
        row = {
            "dir_name": entry.name,
            "path": str(entry),
            "name": entry.name,
            "description": "",
            "hash": "",
            "link": link_target,
            "issue": "",
        }
        if skill_md.is_file():
            try:
                text = skill_md.read_text(encoding="utf-8", errors="replace")
            except OSError as e:
                row["issue"] = f"读取失败: {e}"
                rows.append(row)
                continue
            fm = parse_frontmatter(text)
            row["name"] = fm.get("name") or entry.name
            row["description"] = fm.get("description", "")
            row["hash"] = sha8(text.encode("utf-8"))
            if not fm.get("description"):
                row["issue"] = "frontmatter 缺 description"
        else:
            row["issue"] = "没有 SKILL.md"
        rows.append(row)
    return rows


def scan_mcp(path: Path) -> list[dict]:
    """从 mcp.json / config.toml 里读出服务器名。只取名字，不碰任何凭据。"""
    if not path.is_file():
        return []
    try:
        if path.suffix == ".json":
            data = json.loads(path.read_text(encoding="utf-8-sig", errors="replace"))
            servers = data.get("mcpServers") or {}
            if not isinstance(servers, dict):
                return []
            return [
                {
                    "name": k,
                    "transport": str((v or {}).get("type", "stdio")),
                    "source": str(path),
                }
                for k, v in servers.items()
            ]
        if path.name == "config.toml":
            # 只认 [mcp_servers.<name>] 这一级表头，不解析内容——
            # config.toml 里有明文 token，读得越少越好。
            text = path.read_text(encoding="utf-8", errors="replace")
            names = re.findall(r"^\[mcp_servers\.([A-Za-z0-9_\-]+)\]\s*$", text, re.M)
            return [{"name": n, "transport": "?", "source": str(path)} for n in names]
    except (OSError, json.JSONDecodeError) as e:
        return [{"name": f"<解析失败>", "transport": str(e)[:40], "source": str(path)}]
    return []


def build_report() -> str:
    per_host: dict[str, dict[str, list]] = {}
    for host, cfg in HOSTS.items():
        skills = []
        for d in cfg["skills"]:
            for row in scan_skill_dir(d):
                row["host_dir"] = str(d)
                skills.append(row)
        mcps = []
        for f in cfg["mcp"]:
            mcps.extend(scan_mcp(f))
        per_host[host] = {"skills": skills, "mcp": mcps}

    # 按目录名聚合，找跨宿主重复 / 分叉
    # 同一个真实路径只算一处来源：Codex 用符号链接挂 .cc-switch 的条目，
    # 不这么合并的话会报出「Codex + Codex」这种假重复。
    by_name: dict[str, list[dict]] = {}
    for host, v in per_host.items():
        for row in v["skills"]:
            by_name.setdefault(row["dir_name"], []).append({**row, "host": host})

    deduped: dict[str, list[dict]] = {}
    for name, rows in by_name.items():
        seen: dict[str, dict] = {}
        for r in rows:
            key = r["link"] or r["path"]
            # 保留非链接的那一份做代表，且宿主名去重
            if key not in seen or (seen[key]["link"] and not r["link"]):
                seen[key] = r
        uniq = []
        used_hosts = set()
        for r in seen.values():
            host = r["host"]
            if host in used_hosts:
                continue
            used_hosts.add(host)
            uniq.append(r)
        deduped[name] = uniq
    by_name = deduped

    dupes = {n: rows for n, rows in by_name.items() if len(rows) > 1}

    total = sum(len(v["skills"]) for v in per_host.values())
    lines: list[str] = []
    add = lines.append

    add("# 本机 agent / skills / MCP 索引")
    add("")
    add("> 由 `tools/inventory/scan.py` 生成，**只读扫描**。"
        "重跑即可更新，不要手改本文件。")
    add("")
    add("## 概览")
    add("")
    add("| 宿主 | skills | MCP |")
    add("|---|---:|---:|")
    for host, v in per_host.items():
        if not v["skills"] and not v["mcp"]:
            add(f"| {host} | 0（无目录） | 0 |")
            continue
        add(f"| {host} | {len(v['skills'])} | {len(v['mcp'])} |")
    add(f"| **合计** | **{total}** | **{sum(len(v['mcp']) for v in per_host.values())}** |")
    add("")
    add(f"目录总数 {total}，去重后 {len(by_name)} 个名字。")
    add("")

    add("## 重复与分叉")
    add("")
    mirror_rows = scan_mirror_sets()
    forked = [r for r in mirror_rows if not r["same"]]
    if mirror_rows:
        add("### 源码 → 镜像")
        add("")
        for label in {r["set"] for r in mirror_rows}:
            group = [r for r in mirror_rows if r["set"] == label]
            bad = [r for r in group if not r["same"]]
            if bad:
                add(f"**{label}**：{len(group)} 个同名，**{len(bad)} 个已分叉**")
            else:
                add(f"**{label}**：{len(group)} 个同名，全部一致")
            add("")
            if bad:
                add("| skill | 源指纹 | 镜像指纹 | 该改哪个 |")
                add("|---|---|---|---|")
                for r in bad:
                    add(
                        f"| `{r['name']}` | {r['src_fp']} | {r['mirror_fp']} "
                        f"| 先 `diff` 再决定，别直接覆盖 |"
                    )
                add("")

    add("### 跨宿主同名")
    add("")
    if not dupes:
        add("没有跨宿主重复的 skill。")
    else:
        add("| skill | 出现在 | SKILL.md |")
        add("|---|---|---|")
        for name, rows in sorted(dupes.items()):
            hosts = " + ".join(r["host"] for r in rows)
            hashes = {r["hash"] for r in rows if r["hash"]}
            verdict = "相同" if len(hashes) <= 1 else "**不同**"
            add(f"| `{name}` | {hosts} | {verdict} |")
    add("")

    add("## MCP")
    add("")
    any_mcp = False
    for host, v in per_host.items():
        if not v["mcp"]:
            continue
        any_mcp = True
        add(f"### {host}")
        add("")
        add("| 名字 | 传输 | 配置位置 |")
        add("|---|---|---|")
        for m in v["mcp"]:
            add(f"| `{m['name']}` | {m['transport']} | `{m['source']}` |")
        add("")
    if not any_mcp:
        add("没有找到 MCP 配置。")
    add("")

    add("## 全部 skills")
    add("")
    for host, v in per_host.items():
        if not v["skills"]:
            continue
        add(f"### {host}（{len(v['skills'])} 个）")
        add("")
        add("| 目录名 | 标题 | 说明 | 备注 |")
        add("|---|---|---|---|")
        for row in v["skills"]:
            desc = row["description"].replace("|", "\\|")
            if len(desc) > 110:
                desc = desc[:110].rstrip() + "…"
            name = row["name"].replace("|", "\\|")
            note = row["issue"]
            if row["link"]:
                note = (note + " " if note else "") + f"→ 链接 `{row['link']}`"
            add(f"| `{row['dir_name']}` | {name} | {desc} | {note} |")
        add("")

    return "\n".join(lines) + "\n"


def main() -> int:
    ap = argparse.ArgumentParser(description="盘点本机 agent 工具，只读")
    ap.add_argument("-o", "--output", help="输出路径，默认 tools/inventory/inventory.md")
    ap.add_argument("--stdout", action="store_true", help="打到终端，不写文件")
    args = ap.parse_args()

    report = build_report()
    if args.stdout:
        sys.stdout.write(report)
        return 0

    out = Path(args.output) if args.output else Path(__file__).with_name("inventory.md")
    out.parent.mkdir(parents=True, exist_ok=True)
    # Windows 控制台默认 GBK，写文件一律 UTF-8
    out.write_text(report, encoding="utf-8", newline="\n")
    print(f"已写入 {out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

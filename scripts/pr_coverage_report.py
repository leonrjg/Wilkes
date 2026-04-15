#!/usr/bin/env python3

from __future__ import annotations

import argparse
import json
import subprocess
import sys
import xml.etree.ElementTree as ET
from dataclasses import dataclass
from pathlib import Path


@dataclass
class Metric:
    label: str
    covered: int
    total: int

    @property
    def pct(self) -> float:
        if self.total == 0:
            return 100.0
        return (self.covered / self.total) * 100.0


@dataclass
class CoverageData:
    overall_metrics: list[Metric]
    line_hits: dict[str, dict[int, bool]]


def normalize_repo_path(path: str, repo_root: Path) -> str:
    value = path.replace("\\", "/").strip()
    repo_prefix = repo_root.as_posix().rstrip("/")
    if value.startswith(repo_prefix + "/"):
        value = value[len(repo_prefix) + 1 :]
    while value.startswith("./"):
        value = value[2:]
    while value.startswith("/"):
        value = value[1:]
    return value


def load_frontend_coverage(
    summary_path: Path, cobertura_path: Path, repo_root: Path
) -> CoverageData:
    summary = json.loads(summary_path.read_text())["total"]
    overall_metrics = [
      Metric("Lines", summary["lines"]["covered"], summary["lines"]["total"]),
      Metric("Statements", summary["statements"]["covered"], summary["statements"]["total"]),
      Metric("Functions", summary["functions"]["covered"], summary["functions"]["total"]),
      Metric("Branches", summary["branches"]["covered"], summary["branches"]["total"]),
    ]
    return CoverageData(
        overall_metrics=overall_metrics,
        line_hits=parse_cobertura_lines(cobertura_path, repo_root),
    )


def load_rust_coverage(
    summary_path: Path, cobertura_path: Path, repo_root: Path
) -> CoverageData:
    summary = json.loads(summary_path.read_text())["data"][0]["totals"]
    overall_metrics = [
        Metric("Lines", summary["lines"]["count"], summary["lines"]["total"]),
        Metric("Regions", summary["regions"]["count"], summary["regions"]["total"]),
        Metric("Functions", summary["functions"]["count"], summary["functions"]["total"]),
    ]
    return CoverageData(
        overall_metrics=overall_metrics,
        line_hits=parse_cobertura_lines(cobertura_path, repo_root),
    )


def parse_cobertura_lines(cobertura_path: Path, repo_root: Path) -> dict[str, dict[int, bool]]:
    tree = ET.parse(cobertura_path)
    root = tree.getroot()
    results: dict[str, dict[int, bool]] = {}
    source_roots = []

    for source in root.findall("./sources/source"):
        if source.text:
            source_roots.append(Path(source.text.strip()))

    if not source_roots:
        source_roots = [repo_root]

    for cls in root.findall(".//class"):
        filename = cls.attrib.get("filename")
        if not filename:
            continue
        normalized_candidates = []
        filename_path = Path(filename)
        if filename_path.is_absolute():
            normalized_candidates.append(normalize_repo_path(str(filename_path), repo_root))
        else:
            normalized_candidates.extend(
                normalize_repo_path(str(source_root / filename_path), repo_root)
                for source_root in source_roots
            )

        for line in cls.findall("./lines/line"):
            number = int(line.attrib["number"])
            hits = int(line.attrib.get("hits", "0"))
            for normalized in normalized_candidates:
                file_hits = results.setdefault(normalized, {})
                file_hits[number] = file_hits.get(number, False) or hits > 0

    return results


def collect_changed_lines(repo_root: Path, base_sha: str, head_sha: str) -> dict[str, set[int]]:
    diff = subprocess.run(
        [
            "git",
            "diff",
            "--unified=0",
            "--no-renames",
            base_sha,
            head_sha,
        ],
        cwd=repo_root,
        capture_output=True,
        text=True,
        check=True,
    ).stdout.splitlines()

    changed: dict[str, set[int]] = {}
    current_file: str | None = None
    new_line = 0

    for raw in diff:
        if raw.startswith("+++ b/"):
            current_file = raw[6:]
            changed.setdefault(current_file, set())
            continue
        if raw.startswith("@@"):
            parts = raw.split(" ")
            new_hunk = next(part for part in parts if part.startswith("+"))
            start_text = new_hunk[1:].split(",")[0]
            new_line = int(start_text)
            continue
        if current_file is None or raw.startswith("---") or raw.startswith("diff --git"):
            continue
        if raw.startswith("+"):
            changed[current_file].add(new_line)
            new_line += 1
        elif raw.startswith("-"):
            continue
        else:
            new_line += 1

    return {path: lines for path, lines in changed.items() if lines}


def compute_diff_metric(
    label: str, changed_lines: dict[str, set[int]], line_hits: dict[str, dict[int, bool]]
) -> Metric | None:
    covered = 0
    total = 0
    for path, line_numbers in changed_lines.items():
        file_hits = line_hits.get(path)
        if not file_hits:
            continue
        for line_number in line_numbers:
            hit = file_hits.get(line_number)
            if hit is None:
                continue
            total += 1
            if hit:
                covered += 1
    if total == 0:
        return None
    return Metric(label, covered, total)


def format_metric_table(metrics: list[Metric]) -> str:
    lines = [
        "| Metric | Coverage | Covered / Total |",
        "| --- | ---: | ---: |",
    ]
    for metric in metrics:
        lines.append(
            f"| {metric.label} | {metric.pct:.2f}% | {metric.covered} / {metric.total} |"
        )
    return "\n".join(lines)


def format_diff_line(metric: Metric | None) -> str:
    if metric is None:
        return "No changed lines matched instrumented coverage files."
    return f"{metric.pct:.2f}% ({metric.covered} / {metric.total} changed lines)"


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repo-root", required=True)
    parser.add_argument("--base-sha", required=True)
    parser.add_argument("--head-sha", required=True)
    parser.add_argument("--ui-summary", required=True)
    parser.add_argument("--ui-cobertura", required=True)
    parser.add_argument("--rust-summary", required=True)
    parser.add_argument("--rust-cobertura", required=True)
    parser.add_argument("--run-url", required=True)
    parser.add_argument("--output", required=True)
    args = parser.parse_args()

    repo_root = Path(args.repo_root).resolve()
    frontend = load_frontend_coverage(
        Path(args.ui_summary), Path(args.ui_cobertura), repo_root
    )
    rust = load_rust_coverage(Path(args.rust_summary), Path(args.rust_cobertura), repo_root)
    changed_lines = collect_changed_lines(repo_root, args.base_sha, args.head_sha)

    frontend_diff = compute_diff_metric("Changed lines", changed_lines, frontend.line_hits)
    rust_diff = compute_diff_metric("Changed lines", changed_lines, rust.line_hits)

    combined_covered = 0
    combined_total = 0
    for metric in [rust_diff, frontend_diff]:
        if metric is None:
            continue
        combined_covered += metric.covered
        combined_total += metric.total
    combined_diff = (
        Metric("Changed lines", combined_covered, combined_total)
        if combined_total > 0
        else None
    )

    body = "\n".join(
        [
            "<!-- wilkes-pr-coverage -->",
            "## PR Coverage",
            "",
            "### Diff Coverage",
            "",
            f"- Combined: {format_diff_line(combined_diff)}",
            f"- Rust: {format_diff_line(rust_diff)}",
            f"- Frontend: {format_diff_line(frontend_diff)}",
            "",
            "### Overall Coverage",
            "",
            "#### Rust",
            "",
            format_metric_table(rust.overall_metrics),
            "",
            "#### Frontend",
            "",
            format_metric_table(frontend.overall_metrics),
            "",
            f"[Open this workflow run]({args.run_url}) to download the coverage artifacts for file-level details.",
            "",
        ]
    )

    Path(args.output).write_text(body)
    return 0


if __name__ == "__main__":
    sys.exit(main())

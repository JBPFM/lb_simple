#!/usr/bin/env python3
from __future__ import annotations

import argparse
import csv
import math
import re
import statistics
from dataclasses import dataclass
from pathlib import Path
from typing import Iterable


KNOWN_MODES = ("baseline", "lb_simple")

REST_RE = re.compile(
    r"^(?P<benchmark>.+)_t(?P<threads>\d+)_r(?P<run>\d+)_(?P<ts>\d{8}_\d{6})\.txt$"
)

BENCH_LINE_RE_TEMPLATE = r"(?m)^\s*{bench}\s*:\s*(?P<micros>[0-9]*\.?[0-9]+)\s+micros/op\b"

STAT_LINE_RE = re.compile(
    r"(?m)^@(?P<map>\w+)\[(?P<uaddr>0x[0-9a-fA-F]+)\]:\s+count\s+(?P<count>\d+),\s+average\s+(?P<avg>\d+),\s+total\s+(?P<total>\d+)\s*$"
)


def mean_stdev(values: list[float]) -> tuple[float | None, float | None]:
    if not values:
        return None, None
    if len(values) == 1:
        return values[0], 0.0
    return statistics.mean(values), statistics.stdev(values)


def fmt_float(v: float | None, digits: int = 3) -> str:
    if v is None:
        return ""
    if math.isfinite(v):
        return f"{v:.{digits}f}"
    return ""


def fmt_int(v: int | None) -> str:
    if v is None:
        return ""
    return str(v)


@dataclass(frozen=True)
class ParsedRun:
    path: Path
    mode: str
    benchmark: str
    threads: int
    run: int
    timestamp: str
    micros_per_op: float | None
    wait_count: int | None
    wait_total_ns: int | None
    wake_count: int | None
    wake_total_ns: int | None
    do_wait_count: int | None
    do_wait_total_ns: int | None
    parse_error: str | None

    def wait_avg_ns(self) -> float | None:
        if not self.wait_count or self.wait_total_ns is None:
            return None
        return self.wait_total_ns / self.wait_count

    def wake_avg_ns(self) -> float | None:
        if not self.wake_count or self.wake_total_ns is None:
            return None
        return self.wake_total_ns / self.wake_count

    def do_wait_avg_ns(self) -> float | None:
        if not self.do_wait_count or self.do_wait_total_ns is None:
            return None
        return self.do_wait_total_ns / self.do_wait_count


def parse_one_result(path: Path) -> ParsedRun:
    mode = None
    rest = None
    for m in KNOWN_MODES:
        prefix = f"{m}_"
        if path.name.startswith(prefix):
            mode = m
            rest = path.name[len(prefix) :]
            break

    if mode is None or rest is None:
        return ParsedRun(
            path=path,
            mode="",
            benchmark="",
            threads=0,
            run=0,
            timestamp="",
            micros_per_op=None,
            wait_count=None,
            wait_total_ns=None,
            wake_count=None,
            wake_total_ns=None,
            do_wait_count=None,
            do_wait_total_ns=None,
            parse_error="unknown mode or filename does not start with '<mode>_'",
        )

    rm = REST_RE.match(rest)
    if not rm:
        return ParsedRun(
            path=path,
            mode=mode,
            benchmark="",
            threads=0,
            run=0,
            timestamp="",
            micros_per_op=None,
            wait_count=None,
            wait_total_ns=None,
            wake_count=None,
            wake_total_ns=None,
            do_wait_count=None,
            do_wait_total_ns=None,
            parse_error="filename does not match '<benchmark>_t<threads>_r<run>_<YYYYMMDD>_<HHMMSS>.txt'",
        )

    benchmark = rm.group("benchmark")
    threads = int(rm.group("threads"))
    run = int(rm.group("run"))
    ts = rm.group("ts")

    text = path.read_text(errors="replace")

    bench_re = re.compile(BENCH_LINE_RE_TEMPLATE.format(bench=re.escape(benchmark)))
    bench_m = bench_re.search(text)
    micros_per_op = float(bench_m.group("micros")) if bench_m else None

    sums: dict[str, dict[str, int]] = {
        "wait": {"count": 0, "total": 0},
        "wake": {"count": 0, "total": 0},
        "do_wait": {"count": 0, "total": 0},
    }

    found_any_stat = False
    for sm in STAT_LINE_RE.finditer(text):
        found_any_stat = True
        map_name = sm.group("map")
        count = int(sm.group("count"))
        total = int(sm.group("total"))

        if map_name.startswith("wait_"):
            key = "wait"
        elif map_name.startswith("wake_"):
            key = "wake"
        elif map_name.startswith("do_wait_"):
            key = "do_wait"
        else:
            continue

        sums[key]["count"] += count
        sums[key]["total"] += total

    err = None
    if micros_per_op is None:
        err = f"missing benchmark micros/op line for {benchmark!r}"
    if not found_any_stat:
        err = (err + "; " if err else "") + "missing futex stats output"

    return ParsedRun(
        path=path,
        mode=mode,
        benchmark=benchmark,
        threads=threads,
        run=run,
        timestamp=ts,
        micros_per_op=micros_per_op,
        wait_count=sums["wait"]["count"] or None,
        wait_total_ns=sums["wait"]["total"] or None,
        wake_count=sums["wake"]["count"] or None,
        wake_total_ns=sums["wake"]["total"] or None,
        do_wait_count=sums["do_wait"]["count"] or None,
        do_wait_total_ns=sums["do_wait"]["total"] or None,
        parse_error=err,
    )


def iter_result_files(results_dir: Path) -> Iterable[Path]:
    for p in sorted(results_dir.glob("*.txt")):
        if p.is_file():
            yield p


def write_runs_csv(runs: list[ParsedRun], out_csv: Path) -> None:
    out_csv.parent.mkdir(parents=True, exist_ok=True)
    with out_csv.open("w", newline="") as f:
        w = csv.writer(f)
        w.writerow(
            [
                "file",
                "mode",
                "benchmark",
                "threads",
                "run",
                "timestamp",
                "micros_per_op",
                "wait_count",
                "wait_total_ns",
                "wait_avg_ns",
                "wake_count",
                "wake_total_ns",
                "wake_avg_ns",
                "do_wait_count",
                "do_wait_total_ns",
                "do_wait_avg_ns",
                "parse_error",
            ]
        )
        for r in runs:
            w.writerow(
                [
                    r.path.name,
                    r.mode,
                    r.benchmark,
                    r.threads,
                    r.run,
                    r.timestamp,
                    fmt_float(r.micros_per_op, 6),
                    fmt_int(r.wait_count),
                    fmt_int(r.wait_total_ns),
                    fmt_float(r.wait_avg_ns(), 3),
                    fmt_int(r.wake_count),
                    fmt_int(r.wake_total_ns),
                    fmt_float(r.wake_avg_ns(), 3),
                    fmt_int(r.do_wait_count),
                    fmt_int(r.do_wait_total_ns),
                    fmt_float(r.do_wait_avg_ns(), 3),
                    r.parse_error or "",
                ]
            )


@dataclass(frozen=True)
class AggregateRow:
    mode: str
    benchmark: str
    threads: int
    n: int
    micros_mean: float | None
    micros_stdev: float | None
    wait_count_mean: float | None
    wait_count_stdev: float | None
    wait_total_ms_mean: float | None
    wait_total_ms_stdev: float | None
    wake_count_mean: float | None
    wake_count_stdev: float | None
    wake_total_ms_mean: float | None
    wake_total_ms_stdev: float | None
    do_wait_count_mean: float | None
    do_wait_count_stdev: float | None
    do_wait_total_ms_mean: float | None
    do_wait_total_ms_stdev: float | None


def aggregate_runs(runs: list[ParsedRun]) -> list[AggregateRow]:
    groups: dict[tuple[str, str, int], list[ParsedRun]] = {}
    for r in runs:
        if r.parse_error:
            continue
        key = (r.mode, r.benchmark, r.threads)
        groups.setdefault(key, []).append(r)

    out: list[AggregateRow] = []
    for (mode, benchmark, threads), rs in sorted(groups.items()):
        micros = [r.micros_per_op for r in rs if r.micros_per_op is not None]
        wait_counts = [float(r.wait_count) for r in rs if r.wait_count is not None]
        wait_ms = [
            (r.wait_total_ns / 1_000_000.0)
            for r in rs
            if r.wait_total_ns is not None
        ]
        wake_counts = [float(r.wake_count) for r in rs if r.wake_count is not None]
        wake_ms = [
            (r.wake_total_ns / 1_000_000.0)
            for r in rs
            if r.wake_total_ns is not None
        ]
        do_wait_counts = [float(r.do_wait_count) for r in rs if r.do_wait_count is not None]
        do_wait_ms = [
            (r.do_wait_total_ns / 1_000_000.0)
            for r in rs
            if r.do_wait_total_ns is not None
        ]

        micros_mean, micros_stdev = mean_stdev([float(x) for x in micros])
        wait_count_mean, wait_count_stdev = mean_stdev(wait_counts)
        wait_mean, wait_stdev = mean_stdev(wait_ms)
        wake_count_mean, wake_count_stdev = mean_stdev(wake_counts)
        wake_mean, wake_stdev = mean_stdev(wake_ms)
        do_wait_count_mean, do_wait_count_stdev = mean_stdev(do_wait_counts)
        do_wait_mean, do_wait_stdev = mean_stdev(do_wait_ms)

        out.append(
            AggregateRow(
                mode=mode,
                benchmark=benchmark,
                threads=threads,
                n=len(rs),
                micros_mean=micros_mean,
                micros_stdev=micros_stdev,
                wait_count_mean=wait_count_mean,
                wait_count_stdev=wait_count_stdev,
                wait_total_ms_mean=wait_mean,
                wait_total_ms_stdev=wait_stdev,
                wake_count_mean=wake_count_mean,
                wake_count_stdev=wake_count_stdev,
                wake_total_ms_mean=wake_mean,
                wake_total_ms_stdev=wake_stdev,
                do_wait_count_mean=do_wait_count_mean,
                do_wait_count_stdev=do_wait_count_stdev,
                do_wait_total_ms_mean=do_wait_mean,
                do_wait_total_ms_stdev=do_wait_stdev,
            )
        )
    return out


def write_aggregate_csv(rows: list[AggregateRow], out_csv: Path) -> None:
    out_csv.parent.mkdir(parents=True, exist_ok=True)
    with out_csv.open("w", newline="") as f:
        w = csv.writer(f)
        w.writerow(
            [
                "mode",
                "benchmark",
                "threads",
                "n",
                "micros_per_op_mean",
                "micros_per_op_stdev",
                "wait_count_mean",
                "wait_count_stdev",
                "wait_total_ms_mean",
                "wait_total_ms_stdev",
                "wake_count_mean",
                "wake_count_stdev",
                "wake_total_ms_mean",
                "wake_total_ms_stdev",
                "do_wait_count_mean",
                "do_wait_count_stdev",
                "do_wait_total_ms_mean",
                "do_wait_total_ms_stdev",
            ]
        )
        for r in rows:
            w.writerow(
                [
                    r.mode,
                    r.benchmark,
                    r.threads,
                    r.n,
                    fmt_float(r.micros_mean, 6),
                    fmt_float(r.micros_stdev, 6),
                    fmt_float(r.wait_count_mean, 6),
                    fmt_float(r.wait_count_stdev, 6),
                    fmt_float(r.wait_total_ms_mean, 6),
                    fmt_float(r.wait_total_ms_stdev, 6),
                    fmt_float(r.wake_count_mean, 6),
                    fmt_float(r.wake_count_stdev, 6),
                    fmt_float(r.wake_total_ms_mean, 6),
                    fmt_float(r.wake_total_ms_stdev, 6),
                    fmt_float(r.do_wait_count_mean, 6),
                    fmt_float(r.do_wait_count_stdev, 6),
                    fmt_float(r.do_wait_total_ms_mean, 6),
                    fmt_float(r.do_wait_total_ms_stdev, 6),
                ]
            )


def safe_div(a: float | None, b: float | None) -> float | None:
    if a is None or b is None or b == 0:
        return None
    return a / b


def pct_change(new: float | None, old: float | None) -> float | None:
    if new is None or old is None or old == 0:
        return None
    return (new - old) / old * 100.0


def write_compare_csv(agg: list[AggregateRow], out_csv: Path) -> None:
    by_key: dict[tuple[str, int], dict[str, AggregateRow]] = {}
    for row in agg:
        key = (row.benchmark, row.threads)
        by_key.setdefault(key, {})[row.mode] = row

    out_csv.parent.mkdir(parents=True, exist_ok=True)
    with out_csv.open("w", newline="") as f:
        w = csv.writer(f)
        w.writerow(
            [
                "benchmark",
                "threads",
                "baseline_micros_mean",
                "lb_simple_micros_mean",
                "micros_ratio_lb_over_base",
                "micros_pct_change",
                "baseline_wait_count_mean",
                "lb_simple_wait_count_mean",
                "wait_count_ratio_lb_over_base",
                "wait_count_pct_change",
                "baseline_wait_ms_mean",
                "lb_simple_wait_ms_mean",
                "wait_ms_ratio_lb_over_base",
                "wait_ms_pct_change",
                "baseline_wake_count_mean",
                "lb_simple_wake_count_mean",
                "wake_count_ratio_lb_over_base",
                "wake_count_pct_change",
                "baseline_wake_ms_mean",
                "lb_simple_wake_ms_mean",
                "wake_ms_ratio_lb_over_base",
                "wake_ms_pct_change",
                "baseline_do_wait_count_mean",
                "lb_simple_do_wait_count_mean",
                "do_wait_count_ratio_lb_over_base",
                "do_wait_count_pct_change",
                "baseline_do_wait_ms_mean",
                "lb_simple_do_wait_ms_mean",
                "do_wait_ms_ratio_lb_over_base",
                "do_wait_ms_pct_change",
            ]
        )

        for (benchmark, threads), modes in sorted(by_key.items()):
            b = modes.get("baseline")
            l = modes.get("lb_simple")
            w.writerow(
                [
                    benchmark,
                    threads,
                    fmt_float(b.micros_mean, 6) if b else "",
                    fmt_float(l.micros_mean, 6) if l else "",
                    fmt_float(safe_div(l.micros_mean if l else None, b.micros_mean if b else None), 6),
                    fmt_float(pct_change(l.micros_mean if l else None, b.micros_mean if b else None), 3),
                    fmt_float(b.wait_count_mean, 6) if b else "",
                    fmt_float(l.wait_count_mean, 6) if l else "",
                    fmt_float(safe_div(l.wait_count_mean if l else None, b.wait_count_mean if b else None), 6),
                    fmt_float(pct_change(l.wait_count_mean if l else None, b.wait_count_mean if b else None), 3),
                    fmt_float(b.wait_total_ms_mean, 6) if b else "",
                    fmt_float(l.wait_total_ms_mean, 6) if l else "",
                    fmt_float(
                        safe_div(l.wait_total_ms_mean if l else None, b.wait_total_ms_mean if b else None), 6
                    ),
                    fmt_float(pct_change(l.wait_total_ms_mean if l else None, b.wait_total_ms_mean if b else None), 3),
                    fmt_float(b.wake_count_mean, 6) if b else "",
                    fmt_float(l.wake_count_mean, 6) if l else "",
                    fmt_float(safe_div(l.wake_count_mean if l else None, b.wake_count_mean if b else None), 6),
                    fmt_float(pct_change(l.wake_count_mean if l else None, b.wake_count_mean if b else None), 3),
                    fmt_float(b.wake_total_ms_mean, 6) if b else "",
                    fmt_float(l.wake_total_ms_mean, 6) if l else "",
                    fmt_float(
                        safe_div(l.wake_total_ms_mean if l else None, b.wake_total_ms_mean if b else None), 6
                    ),
                    fmt_float(pct_change(l.wake_total_ms_mean if l else None, b.wake_total_ms_mean if b else None), 3),
                    fmt_float(b.do_wait_count_mean, 6) if b else "",
                    fmt_float(l.do_wait_count_mean, 6) if l else "",
                    fmt_float(safe_div(l.do_wait_count_mean if l else None, b.do_wait_count_mean if b else None), 6),
                    fmt_float(pct_change(l.do_wait_count_mean if l else None, b.do_wait_count_mean if b else None), 3),
                    fmt_float(b.do_wait_total_ms_mean, 6) if b else "",
                    fmt_float(l.do_wait_total_ms_mean, 6) if l else "",
                    fmt_float(
                        safe_div(l.do_wait_total_ms_mean if l else None, b.do_wait_total_ms_mean if b else None), 6
                    ),
                    fmt_float(
                        pct_change(l.do_wait_total_ms_mean if l else None, b.do_wait_total_ms_mean if b else None), 3
                    ),
                ]
            )


def write_markdown_report(
    runs: list[ParsedRun],
    agg: list[AggregateRow],
    compare_csv: Path,
    out_md: Path,
) -> None:
    errors = [r for r in runs if r.parse_error]
    total = len(runs)
    ok = total - len(errors)

    out_md.parent.mkdir(parents=True, exist_ok=True)
    with out_md.open("w") as f:
        f.write("# locktrace db_bench analysis\n\n")
        f.write(f"- Parsed files: {ok}/{total}\n")
        if errors:
            f.write(f"- Parse errors: {len(errors)} (see `runs.csv`)\n")
        f.write(f"- Compare CSV: `{compare_csv}`\n\n")

        f.write("## Aggregated (mean ± stdev)\n\n")
        f.write(
            "| mode | benchmark | threads | n | micros/op | wait_count | wait_total_ms | wake_count | wake_total_ms | do_wait_count | do_wait_total_ms |\n"
        )
        f.write("|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|\n")
        for r in agg:
            f.write(
                "| {mode} | {benchmark} | {threads} | {n} | {micros} | {wait_cnt} | {wait_ms} | {wake_cnt} | {wake_ms} | {do_wait_cnt} | {do_wait_ms} |\n".format(
                    mode=r.mode,
                    benchmark=r.benchmark,
                    threads=r.threads,
                    n=r.n,
                    micros=(
                        f"{fmt_float(r.micros_mean, 6)} ± {fmt_float(r.micros_stdev, 6)}"
                        if r.micros_mean is not None
                        else ""
                    ),
                    wait_cnt=(
                        f"{fmt_float(r.wait_count_mean, 3)} ± {fmt_float(r.wait_count_stdev, 3)}"
                        if r.wait_count_mean is not None
                        else ""
                    ),
                    wait_ms=(
                        f"{fmt_float(r.wait_total_ms_mean, 6)} ± {fmt_float(r.wait_total_ms_stdev, 6)}"
                        if r.wait_total_ms_mean is not None
                        else ""
                    ),
                    wake_cnt=(
                        f"{fmt_float(r.wake_count_mean, 3)} ± {fmt_float(r.wake_count_stdev, 3)}"
                        if r.wake_count_mean is not None
                        else ""
                    ),
                    wake_ms=(
                        f"{fmt_float(r.wake_total_ms_mean, 6)} ± {fmt_float(r.wake_total_ms_stdev, 6)}"
                        if r.wake_total_ms_mean is not None
                        else ""
                    ),
                    do_wait_cnt=(
                        f"{fmt_float(r.do_wait_count_mean, 3)} ± {fmt_float(r.do_wait_count_stdev, 3)}"
                        if r.do_wait_count_mean is not None
                        else ""
                    ),
                    do_wait_ms=(
                        f"{fmt_float(r.do_wait_total_ms_mean, 6)} ± {fmt_float(r.do_wait_total_ms_stdev, 6)}"
                        if r.do_wait_total_ms_mean is not None
                        else ""
                    ),
                )
            )

        if errors:
            f.write("\n## Parse errors (first 20)\n\n")
            f.write("| file | error |\n")
            f.write("|---|---|\n")
            for r in errors[:20]:
                f.write(f"| {r.path.name} | {r.parse_error} |\n")


def main() -> int:
    ap = argparse.ArgumentParser(
        description="Analyze locktrace results produced by test/run_locktrace_db_bench.sh"
    )
    ap.add_argument(
        "-i",
        "--input",
        type=Path,
        default=Path("test/results"),
        help="Results directory (default: test/results)",
    )
    ap.add_argument(
        "-o",
        "--output-prefix",
        type=Path,
        default=Path("test/results/locktrace_db_bench"),
        help="Output prefix path (default: test/results/locktrace_db_bench)",
    )
    ap.add_argument(
        "--no-markdown",
        action="store_true",
        help="Do not write markdown report",
    )
    args = ap.parse_args()

    results_dir: Path = args.input
    if not results_dir.exists():
        raise SystemExit(f"results dir not found: {results_dir}")

    runs = [parse_one_result(p) for p in iter_result_files(results_dir)]
    runs_csv = args.output_prefix.with_suffix(".runs.csv")
    agg_csv = args.output_prefix.with_suffix(".aggregate.csv")
    compare_csv = args.output_prefix.with_suffix(".compare.csv")
    report_md = args.output_prefix.with_suffix(".report.md")

    write_runs_csv(runs, runs_csv)
    agg = aggregate_runs(runs)
    write_aggregate_csv(agg, agg_csv)
    write_compare_csv(agg, compare_csv)
    if not args.no_markdown:
        write_markdown_report(runs, agg, compare_csv, report_md)

    ok = sum(1 for r in runs if not r.parse_error)
    print(f"parsed {ok}/{len(runs)} files")
    print(f"wrote {runs_csv}")
    print(f"wrote {agg_csv}")
    print(f"wrote {compare_csv}")
    if not args.no_markdown:
        print(f"wrote {report_md}")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())

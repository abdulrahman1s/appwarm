#!/usr/bin/env python3
"""Measure Niri window appearance for repeated, untraced desktop launches.

Examples:
  python3 bench/launch.py --match dev.zed.Zed --trials 6 -- zeditor --foreground --new
  python3 bench/launch.py --match brave-origin --trials 6 --warm brave -- brave --incognito about:blank

The script terminates only the process group it started. Close existing app
instances first so their single-instance IPC does not absorb the launch.
"""

import argparse
import json
import os
import signal
import statistics
import subprocess
import time


def windows():
    result = subprocess.run(
        ["niri", "msg", "--json", "windows"],
        check=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    return json.loads(result.stdout)


def launch(command, match, baseline):
    started = time.monotonic()
    process = subprocess.Popen(
        command,
        start_new_session=True,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    elapsed = None
    try:
        deadline = started + 30
        while time.monotonic() < deadline:
            for window in windows():
                if window["id"] not in baseline and match in (window.get("app_id") or ""):
                    elapsed = (time.monotonic() - started) * 1000
                    break
            if elapsed is not None:
                break
            if process.poll() is not None:
                raise RuntimeError(f"launcher exited {process.returncode} before a window appeared")
            time.sleep(0.05)
        if elapsed is None:
            raise TimeoutError("no matching window within 30 seconds")
        return elapsed
    finally:
        try:
            os.killpg(process.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait()
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            if not any(
                window["id"] not in baseline and match in (window.get("app_id") or "")
                for window in windows()
            ):
                break
            time.sleep(0.1)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--match", required=True, help="substring of Niri app_id")
    parser.add_argument("--trials", type=int, default=6)
    parser.add_argument("--warm", help="run appwarm warm NAME before alternate trials")
    parser.add_argument("--warmer", default="appwarm", help="appwarm binary to benchmark")
    parser.add_argument("--warm-cache", help="XDG_CACHE_HOME containing test profiles")
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    if args.trials < 2 or not args.command or args.command[0] != "--":
        parser.error("provide at least two trials and a command after --")
    command = args.command[1:]
    results = {"normal": [], "warmed": []}
    for trial in range(args.trials):
        condition = "warmed" if args.warm and trial % 2 else "normal"
        if condition == "warmed":
            env = os.environ.copy()
            if args.warm_cache:
                env["XDG_CACHE_HOME"] = args.warm_cache
            subprocess.run([args.warmer, "warm", args.warm], check=True, env=env)
        baseline = {window["id"] for window in windows()}
        elapsed = launch(command, args.match, baseline)
        results[condition].append(elapsed)
        print(f"{condition} trial {trial + 1}: {elapsed:.1f} ms", flush=True)
        time.sleep(1)
    for condition, samples in results.items():
        if samples:
            print(f"{condition}: median {statistics.median(samples):.1f} ms; samples {len(samples)}")


if __name__ == "__main__":
    main()

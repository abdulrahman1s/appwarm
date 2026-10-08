#!/usr/bin/env python3
"""Check hidden staging and measure reveal latency on patched Niri.

Run only after the patched compositor is active. The shown app stays open so
clipboard, scaling, and first-interaction latency can be checked by hand.
"""

import argparse
import json
import subprocess
import time


def niri_json(command):
    return json.loads(
        subprocess.check_output(["niri", "msg", "--json", command], text=True)
    )


def systemd_property(unit, key):
    return subprocess.check_output(
        ["systemctl", "--user", "show", "-p", key, "--value", unit], text=True
    ).strip()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--name", required=True)
    parser.add_argument("--app-id", required=True)
    parser.add_argument("--warmer", default="appwarm")
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    if not args.command or args.command[0] != "--" or len(args.command) < 2:
        parser.error("provide an application command after --")

    before = niri_json("windows")
    before_ids = {window["id"] for window in before}
    before_focus = next((window["id"] for window in before if window["is_focused"]), None)
    visible_before = {ws["id"] for ws in niri_json("workspaces")}
    unit = f"appwarm-stage-{args.name}.service"

    started = time.perf_counter()
    subprocess.run(
        [args.warmer, "stage", args.name, "--app-id", args.app_id, "--", *args.command[1:]],
        check=True,
    )
    stage_ms = (time.perf_counter() - started) * 1000

    shown = False
    try:
        extended = niri_json("workspaces-with-hidden")
        hidden_ids = {
            ws["id"] for ws in extended
            if ws["name"] == "appwarm-hidden" and ws["is_hidden"]
        }
        assert hidden_ids, "hidden workspace is missing"
        visible_after = {ws["id"] for ws in niri_json("workspaces")}
        assert hidden_ids.isdisjoint(visible_after), "staging workspace leaked into normal IPC"
        assert visible_before == visible_after, "visible workspace list changed during stage"

        windows = niri_json("windows")
        staged = next(
            window for window in windows
            if window["id"] not in before_ids and window["app_id"] == args.app_id
        )
        assert staged["workspace_id"] in hidden_ids, "app window is visible"
        assert not staged["is_focused"], "app stole focus while staging"
        after_focus = next((window["id"] for window in windows if window["is_focused"]), None)
        assert after_focus == before_focus, "focus changed during staging"
        assert systemd_property(unit, "FreezerState") == "frozen", "app was not frozen"
        memory_mib = int(systemd_property(unit, "MemoryCurrent")) / 1024 / 1024

        started = time.perf_counter()
        subprocess.run([args.warmer, "show", args.name], check=True)
        reveal_ms = (time.perf_counter() - started) * 1000
        shown = True

        windows = niri_json("windows")
        revealed = next(window for window in windows if window["id"] == staged["id"])
        assert revealed["is_focused"], "reveal did not focus the same window"
        assert revealed["workspace_id"] not in hidden_ids, "window stayed hidden"

        print(json.dumps({
            "stage_ms": round(stage_ms, 1),
            "reveal_command_ms": round(reveal_ms, 1),
            "staged_memory_mib": round(memory_mib, 1),
            "window_id": staged["id"],
            "focus_preserved_during_stage": True,
        }, indent=2))
        print("Check first-click response, clipboard, scaling, and popups in the open app.")
    finally:
        if not shown:
            subprocess.run([args.warmer, "evict", args.name], check=False)


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
"""
Cookie 对齐验证脚本：
1. 调用 Python browser_cookie3 读取 cookies
2. 调用 Rust cookie-verify 读取 cookies
3. 逐字段对比，输出差异
"""
import json
import os
import subprocess
import sys

sys.path.insert(0, os.path.dirname(__file__))

from gemini_cookies import (
    get_cookies_from_local_browser,
    _discover_chrome_cookie_files,
    _select_preferred_google_cookies,
)
import browser_cookie3

PROJECT_ROOT = os.path.join(os.path.dirname(__file__), "..")
TAURI_TARGET = os.path.join(PROJECT_ROOT, "src-tauri", "target")


def read_python_cookies():
    """Read cookies using Python browser_cookie3 (matching gemini_cookies.py logic)."""
    domain_names = [".google.com", "accounts.google.com", "gemini.google.com"]
    cookie_files, _ = _discover_chrome_cookie_files()

    results = []
    for browser_name, loader, cookie_file, profile_name in cookie_files:
        collected_items = []
        try:
            for dn in domain_names:
                jar = loader(cookie_file=cookie_file, domain_name=dn)
                collected_items.extend(jar)
        except Exception as e:
            results.append({
                "browser": browser_name,
                "profile": profile_name,
                "cookie_file": cookie_file,
                "error": str(e),
            })
            continue

        selected = _select_preferred_google_cookies(collected_items)
        results.append({
            "browser": browser_name,
            "profile": profile_name,
            "cookie_file": cookie_file,
            "raw_count": len(collected_items),
            "selected_count": len(selected),
            "selected": dict(sorted(selected.items())),
        })

    final_cookies = get_cookies_from_local_browser()
    final_sorted = dict(sorted(final_cookies.items()))

    return {
        "browsers": results,
        "final_selected": final_sorted,
        "final_count": len(final_sorted),
    }


def read_rust_cookies():
    """Run Rust cookie-verify binary and parse its JSON output."""
    for profile in ("release", "debug"):
        binary = os.path.join(TAURI_TARGET, profile, "cookie-verify")
        if os.path.exists(binary):
            break
    else:
        print("ERROR: cookie-verify binary not found. Run 'cargo build --bin cookie-verify' in src-tauri/ first.", file=sys.stderr)
        sys.exit(1)

    result = subprocess.run(
        [binary],
        capture_output=True,
        text=True,
        timeout=60,
    )
    if result.returncode != 0:
        print(f"ERROR: cookie-verify exited with code {result.returncode}", file=sys.stderr)
        print(result.stderr, file=sys.stderr)
        sys.exit(1)

    if result.stderr:
        print("[Rust stderr]", file=sys.stderr)
        for line in result.stderr.strip().split("\n"):
            print(f"  {line}", file=sys.stderr)

    return json.loads(result.stdout)


def compare(py_data, rs_data):
    """Compare Python and Rust cookie outputs, report differences."""
    print("\n" + "=" * 60)
    print("COOKIE VERIFICATION COMPARISON")
    print("=" * 60)

    py_final = py_data.get("final_selected", {})
    rs_final = rs_data.get("final_selected", {})

    print(f"\nPython: {len(py_final)} cookies selected")
    print(f"Rust:   {len(rs_final)} cookies selected")

    all_keys = sorted(set(list(py_final.keys()) + list(rs_final.keys())))
    match_count = 0
    mismatch_count = 0
    py_only = []
    rs_only = []

    for key in all_keys:
        py_val = py_final.get(key)
        rs_val = rs_final.get(key)

        if py_val is None:
            rs_only.append(key)
        elif rs_val is None:
            py_only.append(key)
        elif py_val == rs_val:
            match_count += 1
        else:
            mismatch_count += 1
            py_preview = py_val[:40] + "..." if len(py_val) > 40 else py_val
            rs_preview = rs_val[:40] + "..." if len(rs_val) > 40 else rs_val
            print(f"\n  MISMATCH: {key}")
            print(f"    Python: {py_preview}")
            print(f"    Rust:   {rs_preview}")

    if py_only:
        print(f"\n  Python-only cookies ({len(py_only)}):")
        for k in py_only:
            print(f"    - {k}")

    if rs_only:
        print(f"\n  Rust-only cookies ({len(rs_only)}):")
        for k in rs_only:
            print(f"    - {k}")

    print(f"\n--- Summary ---")
    print(f"  Matched:    {match_count}")
    print(f"  Mismatched: {mismatch_count}")
    print(f"  Python-only: {len(py_only)}")
    print(f"  Rust-only:   {len(rs_only)}")

    key_cookies = ["__Secure-1PSID", "__Secure-1PSIDTS", "SID", "HSID", "SSID", "SAPISID"]
    print(f"\n--- Key Cookies ---")
    all_key_match = True
    for kc in key_cookies:
        py_v = py_final.get(kc)
        rs_v = rs_final.get(kc)
        status = "MATCH" if (py_v == rs_v and py_v is not None) else ("MISSING" if py_v is None and rs_v is None else "MISMATCH")
        if status != "MATCH":
            all_key_match = False
        icon = "OK" if status == "MATCH" else "FAIL"
        print(f"  [{icon}] {kc}: {status}")

    print(f"\n{'=' * 60}")
    if mismatch_count == 0 and len(py_only) == 0 and len(rs_only) == 0 and all_key_match:
        print("RESULT: ALL COOKIES MATCH")
    elif mismatch_count == 0 and all_key_match:
        print("RESULT: KEY COOKIES MATCH (minor differences in non-critical cookies)")
    else:
        print("RESULT: DIFFERENCES FOUND - needs investigation")
    print("=" * 60)

    return mismatch_count == 0 and all_key_match


def main():
    print("Step 1: Reading cookies with Python (browser_cookie3)...")
    py_data = read_python_cookies()

    print("\nStep 2: Reading cookies with Rust (cookie-verify)...")
    rs_data = read_rust_cookies()

    success = compare(py_data, rs_data)
    sys.exit(0 if success else 1)


if __name__ == "__main__":
    main()

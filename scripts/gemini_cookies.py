"""
从本机浏览器读取 Google cookies，及账号 email/authuser 映射发现。
"""

import codecs
import json
import os
import platform
import re
import subprocess
from pathlib import Path

try:
    import httpx
except ImportError:
    import sys
    os.system(f"{sys.executable} -m pip install httpx")
    import httpx

try:
    import browser_cookie3
except ImportError:
    import sys
    os.system(f"{sys.executable} -m pip install browser-cookie3")
    import browser_cookie3

from gemini_protocol import GEMINI_BASE, BROWSER_USER_AGENT, BROWSER_ACCEPT_LANGUAGE

GOOGLE_MEDIA_COOKIE_NAMES = [
    "AEC", "__Secure-BUCKET", "SID", "__Secure-1PSID", "__Secure-3PSID",
    "HSID", "SSID", "APISID", "SAPISID", "__Secure-1PAPISID",
    "__Secure-3PAPISID", "NID", "__Secure-1PSIDTS", "__Secure-3PSIDTS",
    "GOOGLE_ABUSE_EXEMPTION", "SIDCC", "__Secure-1PSIDCC", "__Secure-3PSIDCC",
]


def _normalize_cookie_domain(domain):
    return (domain or "").lower().lstrip(".")


def _is_google_domain(domain):
    norm = _normalize_cookie_domain(domain)
    return norm == "google.com" or norm.endswith(".google.com")


def _cookie_domain_priority(domain):
    """优先选择 .google.com，其次 *.google.com，最后其他域。"""
    norm = _normalize_cookie_domain(domain)
    if norm == "google.com":
        return 0
    if norm.endswith(".google.com"):
        return 1
    return 9


def _select_preferred_google_cookies(cookie_items):
    """
    从 cookie 列表中选择更稳定的 Google 会话值。

    cookie_items 支持两种结构：
    - dict: {"name": ..., "value": ..., "domain": ...}
    - Cookie 对象: .name/.value/.domain
    """
    selected = {}
    selected_priority = {}

    for item in cookie_items:
        if isinstance(item, dict):
            name = item.get("name")
            value = item.get("value")
            domain = item.get("domain", "")
        else:
            name = getattr(item, "name", None)
            value = getattr(item, "value", None)
            domain = getattr(item, "domain", "")

        if not name or value is None or not _is_google_domain(domain):
            continue

        prio = _cookie_domain_priority(domain)
        prev_prio = selected_priority.get(name)
        if prev_prio is None or prio < prev_prio:
            selected[name] = value
            selected_priority[name] = prio

    return selected


def _check_keychain_access(service, user):
    """检测 macOS Keychain 中浏览器加密密钥是否可读。返回 (ok, detail)。"""
    if platform.system() != "Darwin":
        return True, ""
    try:
        proc = subprocess.run(
            ["/usr/bin/security", "-q", "find-generic-password", "-w", "-a", user, "-s", service],
            capture_output=True, timeout=10,
        )
        if proc.returncode == 0:
            return True, ""
        stderr = proc.stderr.decode(errors="replace").strip()
        return False, f"Keychain 读取 \"{service}\" 失败 (exit {proc.returncode}): {stderr}"
    except FileNotFoundError:
        return False, "未找到 /usr/bin/security 命令"
    except Exception as e:
        return False, f"Keychain 检测异常: {e}"


def _check_file_readable(path):
    """检测文件是否存在且可读。返回 (ok, detail)。"""
    p = Path(path)
    if not p.exists():
        return False, f"文件不存在: {path}"
    try:
        with open(p, "rb") as f:
            f.read(16)
        return True, ""
    except PermissionError:
        return False, f"权限不足，无法读取: {path} (请在 系统设置→隐私与安全性→完全磁盘访问 中授权本应用)"
    except Exception as e:
        return False, f"无法读取 {path}: {e}"


def _discover_chrome_cookie_files():
    """枚举本机所有可能的 Chrome/Chromium 系 Cookies 文件路径，Network/Cookies 优先。"""
    system = platform.system()

    # (浏览器名, loader 函数名, 基础目录列表)
    browser_specs = []

    if system == "Darwin":
        base = Path.home() / "Library/Application Support"
        browser_specs = [
            ("Chrome", "chrome", [base / f"Google/{ch}" for ch in
                ["Chrome", "Chrome Beta", "Chrome Dev", "Chrome Canary"]]),
            ("Chromium", "chromium", [base / "Chromium"]),
            ("Brave", "brave", [base / "BraveSoftware/Brave-Browser"]),
            ("Edge", "edge", [base / "Microsoft Edge"]),
        ]
    elif system == "Linux":
        suffixes = ["", "-beta", "-unstable"]
        browser_specs = [
            ("Chrome", "chrome",
                [Path.home() / f".config/google-chrome{s}" for s in suffixes]
                + [Path.home() / f".var/app/com.google.Chrome/config/google-chrome{s}" for s in suffixes]),
            ("Chromium", "chromium",
                [Path.home() / ".config/chromium",
                 Path.home() / "snap/chromium/common/chromium"]),
            ("Brave", "brave", [Path.home() / ".config/BraveSoftware/Brave-Browser"]),
            ("Edge", "edge", [Path.home() / ".config/microsoft-edge"]),
        ]
    elif system == "Windows":
        local = Path(os.environ.get("LOCALAPPDATA", ""))
        browser_specs = [
            ("Chrome", "chrome", [local / f"Google/{ch}/User Data" for ch in
                ["Chrome", "Chrome Beta", "Chrome Dev", "Chrome SxS"]]),
            ("Chromium", "chromium", [local / "Chromium/User Data"]),
            ("Brave", "brave", [local / "BraveSoftware/Brave-Browser/User Data"]),
            ("Edge", "edge", [local / "Microsoft/Edge/User Data"]),
        ]

    results = []
    permission_issues = []

    for browser_name, loader_name, base_dirs in browser_specs:
        loader = getattr(browser_cookie3, loader_name, None)
        if loader is None:
            continue
        for base_dir in base_dirs:
            if not base_dir.is_dir():
                # 父目录在、子目录不在 → 可能被 TCC 隐藏
                if base_dir.parent.is_dir():
                    try:
                        siblings = [d.name for d in base_dir.parent.iterdir()]
                        if base_dir.name not in siblings and base_dir.name in ("Chrome", "Google Chrome"):
                            permission_issues.append(
                                f"[{browser_name}] 目录 {base_dir} 不可见，"
                                f"可能被系统隐私保护屏蔽 (请在 系统设置→隐私与安全性→完全磁盘访问 中授权本应用)"
                            )
                    except PermissionError:
                        permission_issues.append(
                            f"[{browser_name}] 无权列出 {base_dir.parent} 目录内容"
                        )
                continue

            # 收集所有 profile 目录
            profile_dirs = []
            for name in ["Default", "Guest Profile", "System Profile"]:
                p = base_dir / name
                if p.is_dir():
                    profile_dirs.append(p)
            profile_dirs += sorted(base_dir.glob("Profile *"))

            if not profile_dirs:
                # 有 Local State 说明浏览器用过，但 Profile 被隐藏
                try:
                    children = [d.name for d in base_dir.iterdir()]
                    if "Default" not in children and (base_dir / "Local State").exists():
                        permission_issues.append(
                            f"[{browser_name}] {base_dir} 下 Profile 目录不可见，"
                            f"可能被系统隐私保护屏蔽 (请在 系统设置→隐私与安全性→完全磁盘访问 中授权本应用)"
                        )
                except PermissionError:
                    permission_issues.append(
                        f"[{browser_name}] 无权列出 {base_dir} 目录内容 (请授权完全磁盘访问)"
                    )

            for pdir in profile_dirs:
                for rel in ["Network/Cookies", "Cookies"]:
                    f = pdir / rel
                    if f.is_file():
                        readable, detail = _check_file_readable(f)
                        if readable:
                            results.append((browser_name, loader, str(f), pdir.name))
                        else:
                            permission_issues.append(f"[{browser_name}/{pdir.name}] {detail}")
                        break
    return results, permission_issues


# 浏览器名 → (Keychain service, Keychain user)
_KEYCHAIN_MAP = {
    "Chrome":   ("Chrome Safe Storage", "Chrome"),
    "Chromium": ("Chromium Safe Storage", "Chromium"),
    "Brave":    ("Brave Safe Storage", "Brave"),
    "Edge":     ("Microsoft Edge Safe Storage", "Microsoft Edge"),
}


def get_cookies_from_local_browser():
    """优先从本机常用浏览器读取 Google/Gemini cookies"""
    print("[*] 尝试从本机浏览器读取 cookies...")

    key_cookies = {"__Secure-1PSID", "__Secure-1PSIDTS"}
    domain_names = [".google.com", "accounts.google.com", "gemini.google.com"]

    cookie_files, permission_issues = _discover_chrome_cookie_files()
    attempted_browsers = set()

    if cookie_files:
        for browser_name, loader, cookie_file, profile_name in cookie_files:
            label = f"{browser_name}/{profile_name}"
            attempted_browsers.add(browser_name)
            try:
                collected_items = []
                for dn in domain_names:
                    jar = loader(cookie_file=cookie_file, domain_name=dn)
                    collected_items.extend(jar)

                collected = _select_preferred_google_cookies(collected_items)
                if not collected:
                    print(f"  - {label}: 未读取到可用 cookie")
                    continue

                if any(k in collected for k in key_cookies):
                    print(f"  - {label}: 成功读取 {len(collected)} 个 cookies")
                    return collected

                print(f"  - {label}: 已读取 {len(collected)} 个 cookies，但缺少关键登录态")
            except Exception as e:
                print(f"  - {label}: 读取失败 ({e})")
    else:
        print("  未发现已知 cookie 文件，回退到 browser_cookie3 默认扫描...")
        fallback_loaders = [
            ("Chrome", getattr(browser_cookie3, "chrome", None)),
            ("Chromium", getattr(browser_cookie3, "chromium", None)),
            ("Brave", getattr(browser_cookie3, "brave", None)),
            ("Edge", getattr(browser_cookie3, "edge", None)),
        ]
        for browser_name, loader in fallback_loaders:
            if loader is None:
                continue
            attempted_browsers.add(browser_name)
            try:
                collected_items = []
                for dn in domain_names:
                    jar = loader(domain_name=dn)
                    collected_items.extend(jar)

                collected = _select_preferred_google_cookies(collected_items)
                if not collected:
                    print(f"  - {browser_name}: 未读取到可用 cookie")
                    continue

                if any(k in collected for k in key_cookies):
                    print(f"  - {browser_name}: 成功读取 {len(collected)} 个 cookies")
                    return collected

                print(f"  - {browser_name}: 已读取 {len(collected)} 个 cookies，但缺少关键登录态")
            except Exception as e:
                print(f"  - {browser_name}: 读取失败 ({e})")

    # ── 读取失败，输出诊断 ──
    # 文件系统权限问题（discover 阶段已收集）
    if permission_issues:
        print("[!] 检测到权限问题:")
        for issue in permission_issues:
            print(f"    {issue}")

    # Keychain 事后诊断：仅检测找到了 cookie 文件的浏览器（非 fallback 路径尝试的所有浏览器）
    if platform.system() == "Darwin" and cookie_files:
        checked = set()
        for browser_name, _, _, _ in cookie_files:
            if browser_name in checked:
                continue
            checked.add(browser_name)
            kc = _KEYCHAIN_MAP.get(browser_name)
            if not kc:
                continue
            ok, detail = _check_keychain_access(kc[0], kc[1])
            if not ok:
                print(f"[!] {detail}")

    return {}


def discover_email_authuser_mapping_via_listaccounts(cookies):
    """
    使用 ListAccounts 接口获取邮箱与 authuser 映射。
    """
    list_accounts_url = "https://accounts.google.com/ListAccounts"
    params = {
        "authuser": "0",
        "listPages": "1",
        "fwput": "10",
        "rdr": "2",
        "pid": "658",
        "gpsia": "1",
        "source": "ogb",
        "atic": "1",
        "mo": "1",
        "mn": "1",
        "hl": "zh-CN",
        "ts": "641",
    }
    headers = {
        "User-Agent": BROWSER_USER_AGENT,
        "Accept-Language": BROWSER_ACCEPT_LANGUAGE,
        "Referer": f"{GEMINI_BASE}/app",
        "Origin": GEMINI_BASE,
    }

    with httpx.Client(cookies=cookies, follow_redirects=True, timeout=30.0, headers=headers) as client:
        resp = client.get(list_accounts_url, params=params)

    if resp.status_code != 200:
        raise RuntimeError(f"ListAccounts HTTP {resp.status_code}")

    m = re.search(r"postMessage\('(.+?)'\s*,\s*'[^']*'\)", resp.text, flags=re.S)
    if not m:
        raise RuntimeError("ListAccounts 响应缺少 postMessage payload")

    payload_raw = m.group(1).replace("\\/", "/")
    payload = codecs.decode(payload_raw, "unicode_escape")
    parsed = json.loads(payload)

    rows = parsed[1] if isinstance(parsed, list) and len(parsed) > 1 and isinstance(parsed[1], list) else []
    result = []
    seen_email = set()
    for row in rows:
        if not isinstance(row, list) or len(row) < 4:
            continue

        email = row[3] if isinstance(row[3], str) else ""
        if not email:
            continue
        email = email.strip().lower()
        if email in seen_email:
            continue
        seen_email.add(email)

        authuser_raw = row[7] if len(row) > 7 else None
        authuser = None
        if isinstance(authuser_raw, int):
            authuser = str(authuser_raw)
        elif isinstance(authuser_raw, str) and authuser_raw.isdigit():
            authuser = authuser_raw

        redirect_url = None
        if authuser is not None:
            redirect_url = f"{GEMINI_BASE}/app" if authuser == "0" else f"{GEMINI_BASE}/u/{authuser}/app"

        result.append({
            "email": email,
            "authuser": authuser,
            "redirect_url": redirect_url,
        })

    return result


def discover_email_authuser_mapping(cookies):
    """通过 ListAccounts 获取本地 cookies 下的账号映射。"""
    return discover_email_authuser_mapping_via_listaccounts(cookies)

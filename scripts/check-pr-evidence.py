#!/usr/bin/env python3
"""Checks that a pull request body shows its testing, and screenshots when the
change touches the window.

Usage: check-pr-evidence.py BODY_FILE CHANGED_FILES_FILE [LABEL ...]
Exits 1 with a message per missing piece; 0 when the body has what it needs.
"""
import re
import sys

# Code that shapes what is on screen: the window and its view models, the
# controller that opens windows and surfaces, the tray, floating-surface
# placement, and icons. A maintainer waives changes here that show nothing.
UI_PATHS = (
    "apps/kestrel/src/window/",
    "apps/kestrel/src/view_model/",
    "apps/kestrel/src/main.rs",
    "apps/kestrel/src/status_notifier.rs",
    "apps/kestrel/src/layer_shell.rs",
    "data/icons/",
)
WAIVER_LABEL = "no-screenshot-needed"
# Text shorter than this, once comments are removed, is not a description.
MIN_TESTING_CHARS = 20
IMAGE = re.compile(
    r"!\[[^\]]*\]\([^)]+\)"  # Markdown image
    r"|<img\s"  # HTML image
    r"|https://github\.com/user-attachments/assets/\S+",  # drag-and-drop upload
    re.IGNORECASE,
)


def section(body: str, title: str) -> str:
    """The text under a `## title` heading, up to the next `## ` heading."""
    match = re.search(
        rf"^##\s+{re.escape(title)}\s*$(.*?)(?=^##\s|\Z)",
        body,
        re.MULTILINE | re.DOTALL | re.IGNORECASE,
    )
    if match is None:
        return ""
    return re.sub(r"<!--.*?-->", "", match.group(1), flags=re.DOTALL).strip()


def problems(body: str, changed: list[str], labels: list[str]) -> list[str]:
    found = []
    if len(section(body, "Testing")) < MIN_TESTING_CHARS:
        found.append(
            "Describe what you ran and what you saw under a '## Testing' heading."
        )
    touches_ui = any(
        path.startswith(UI_PATHS) or path.endswith(".css") for path in changed
    )
    if touches_ui and WAIVER_LABEL not in labels:
        if not IMAGE.search(section(body, "Screenshots")):
            found.append(
                "This change can affect what is on screen: add a screenshot "
                "under a '## Screenshots' heading."
            )
    return found


def main() -> int:
    if len(sys.argv) < 3:
        print(__doc__, file=sys.stderr)
        return 2
    with open(sys.argv[1], encoding="utf-8") as handle:
        body = handle.read()
    with open(sys.argv[2], encoding="utf-8") as handle:
        changed = [line.strip() for line in handle if line.strip()]
    found = problems(body, changed, sys.argv[3:])
    for message in found:
        print(f"::error::{message}")
    if not found:
        print("The pull request describes its testing and includes what it needs.")
    return 1 if found else 0


if __name__ == "__main__":
    sys.exit(main())

# SPDX-License-Identifier: MIT
"""Validate public desktop runtime metadata before packaging a release."""
from __future__ import annotations

from runtime import verify_release_sources


def main() -> int:
    """Print every verified default transport and exit unsuccessfully on drift."""
    for source in verify_release_sources():
        print(f"verified: {source}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

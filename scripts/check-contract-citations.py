#!/usr/bin/env python3
"""Validate the code citations in the frozen app-integration contract.

"Enforced — <citation>" is the contract's entire verifiability story, so a
citation that has drifted is not cosmetic: a peer author following it lands in
the wrong place, or in no place at all.

This checks the two failure modes that can be detected mechanically:

  * a citation whose line range runs past the end of the file it names;
  * a bare basename that matches more than one file in the repo, so the reader
    cannot tell which is meant (``chart_panels.rs`` and ``table_panels.rs`` each
    exist under both ``src/routes/`` and ``src/services/``).

Line-number *accuracy* cannot be checked this way — prefer full path plus symbol
(``src/services/table_data.rs`` (``MAX_TABLE_ROWS``)), which survives refactors.

Exit codes: 0 clean, 1 problems found.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DOC = ROOT / "md" / "design" / "app-integration-contract-v1.md"

CITATION = re.compile(r"`([A-Za-z0-9_/.]+\.rs):(\d+)(?:-(\d+))?`")


def main() -> int:
    if not DOC.exists():
        print(f"contract not found: {DOC}", file=sys.stderr)
        return 1

    by_name: dict[str, list[Path]] = {}
    for path in ROOT.rglob("*.rs"):
        if "/target/" in str(path):
            continue
        by_name.setdefault(path.name, []).append(path)

    text = DOC.read_text()
    past_eof: list[str] = []
    ambiguous: list[str] = []
    ok = 0

    for match in CITATION.finditer(text):
        cite, start_raw, end_raw = match.group(1), match.group(2), match.group(3)
        start = int(start_raw)
        end = int(end_raw) if end_raw else start

        candidate = ROOT / cite
        if not candidate.exists():
            matches = by_name.get(cite.split("/")[-1], [])
            if len(matches) != 1:
                rel = [str(p.relative_to(ROOT)) for p in matches]
                ambiguous.append(
                    f"{cite}:{start}-{end} resolves to {len(matches)} files: {rel or 'none'}"
                )
                continue
            candidate = matches[0]

        line_count = len(candidate.read_text().splitlines())
        if end > line_count:
            past_eof.append(
                f"{cite}:{start}-{end} runs past EOF "
                f"({candidate.relative_to(ROOT)} has {line_count} lines)"
            )
        else:
            ok += 1

    print(f"contract citations resolved and within EOF : {ok}")
    print(f"citations past EOF                         : {len(past_eof)}")
    for problem in past_eof:
        print(f"  PAST-EOF   {problem}")
    print(f"ambiguous citations                        : {len(ambiguous)}")
    for problem in ambiguous:
        print(f"  AMBIGUOUS  {problem}")

    if past_eof or ambiguous:
        print(
            "\nUse a full path, and prefer naming the symbol over a line range.",
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())

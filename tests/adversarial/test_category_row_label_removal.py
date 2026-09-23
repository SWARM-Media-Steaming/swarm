#!/usr/bin/env python3
"""Issue #352: Remove "Categories" label from catalog category row.

The category tiles in CatalogScreen (Movies/Shows/Music) should not be
preceded by a "Categories" text label. The tiles themselves — outlined
genre buttons — are self-explanatory and the label is redundant chrome.

This test verifies:
1. The "Categories" ShelfHeader is removed from CategoryRow
2. CategoryRow still displays the category tiles (LazyRow)
3. The tiles maintain proper styling and spacing
4. Filtering logic remains intact (onClick, selectedGenre tracking)
5. The documentation reflects this UI change
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
CATALOG_SCREEN = ROOT / "clients/tv-android/app/src/main/kotlin/app/swarm/tv/app/ui/screens/CatalogScreen.kt"
TV_CONVENTIONS_SKILL = ROOT / ".claude/skills/tv-client-ui-conventions/SKILL.md"
FEATURE_INVENTORY = ROOT / ".claude/skills/swarm-client-platform-knowledge/references/feature-inventory.md"


def fail(msg: str) -> None:
    print(f"FAIL: {msg}", file=sys.stderr)
    sys.exit(1)


def assert_categories_label_removed() -> None:
    """Verify 'Categories' ShelfHeader is not in CategoryRow."""
    text = CATALOG_SCREEN.read_text()

    # Check that "Categories" label is not in the entire file
    if 'ShelfHeader("Categories"' in text:
        fail('CatalogScreen still has ShelfHeader("Categories", ...) somewhere')

    # Find the section around CategoryRow function
    category_row_start = text.find("private fun CategoryRow(")
    category_row_end = text.find("\nprivate fun CategoryTile(", category_row_start)

    if category_row_start == -1:
        fail("CategoryRow function not found")

    category_row = text[category_row_start:category_row_end]

    # Verify issue #352 is referenced somewhere in the docstring/comments
    # Search backwards from CategoryRow for the docstring
    doc_start = text.rfind("/**", 0, category_row_start)
    doc_end = text.find("*/", doc_start) + 2
    doc_section = text[doc_start:doc_end]

    if "#352" not in doc_section:
        fail("CategoryRow docstring should reference issue #352 for the label removal")


def assert_category_row_structure_intact() -> None:
    """Verify CategoryRow still has all essential components."""
    text = CATALOG_SCREEN.read_text()

    # Find the CategoryRow function
    category_row_start = text.find("private fun CategoryRow(")
    category_row_end = text.find("\nprivate fun CategoryTile(", category_row_start)

    if category_row_start == -1:
        fail("CategoryRow function not found")

    category_row = text[category_row_start:category_row_end]

    # LazyRow should still be present (displays the category tiles)
    if "LazyRow" not in category_row:
        fail("CategoryRow missing LazyRow (category tiles display)")

    # Category tiles should still be rendered
    if "CategoryTile(" in category_row:
        pass  # Found CategoryTile call
    else:
        fail("CategoryRow not rendering CategoryTile composables")

    # Spacing and styling should still be configured
    if "horizontalArrangement = Arrangement.spacedBy" not in category_row:
        fail("CategoryRow missing horizontal spacing configuration")

    # Category selection logic should be intact
    if "onSelect(" not in category_row:
        fail("CategoryRow missing onSelect callback for filtering")

    # Filtering state tracking
    if "selectedGenre" not in category_row:
        fail("CategoryRow not tracking selected genre state")

    # Focus management for TV navigation
    if "focusRequester" not in category_row:
        fail("CategoryRow missing focus management for TV navigation")


def assert_category_tile_styling_preserved() -> None:
    """Verify CategoryTile styling is not affected by label removal."""
    text = CATALOG_SCREEN.read_text()

    # Find CategoryTile function
    tile_start = text.find("private fun CategoryTile(")
    if tile_start == -1:
        fail("CategoryTile function not found")

    # Find the end of the function (next private fun or @Composable private fun)
    tile_end = text.find("\n@Composable", tile_start + 1)
    if tile_end == -1:
        tile_end = text.find("\nprivate fun", tile_start + 1)
    if tile_end == -1:
        tile_end = len(text)

    tile = text[tile_start:tile_end]

    # Visual styling should be intact
    if "Card" not in tile:
        fail("CategoryTile not using Card for styling")

    if "CATEGORY_TILE_SHAPE" not in tile:
        fail("CategoryTile shape styling removed")

    if "CATEGORY_TILE_HEIGHT" not in tile:
        fail("CategoryTile height constant removed")

    # Border styling for the tile outline
    if "Border" not in tile or "BorderStroke" not in tile:
        fail("CategoryTile missing border styling")

    # Selection styling (filled when selected, outlined when not)
    if "selected" not in tile:
        fail("CategoryTile not responding to selection state")

    # Container and content colors
    if "containerColor" not in tile or "contentColor" not in tile:
        fail("CategoryTile missing color configuration")


def assert_documentation_updated() -> None:
    """Verify skill documentation reflects the label removal."""
    skill_text = TV_CONVENTIONS_SKILL.read_text()

    # The TV conventions skill should document that there's no "Categories" label
    if "no section label" not in skill_text.lower():
        if "#352" not in skill_text:
            fail("tv-client-ui-conventions SKILL.md not updated to document label removal")

    # Check feature inventory mentions it
    inventory_text = FEATURE_INVENTORY.read_text()
    if "#352" not in inventory_text:
        fail("feature-inventory.md not updated to reference issue #352")


def main() -> None:
    print("Testing issue #352: Remove 'Categories' label from category row...")

    assert_categories_label_removed()
    print("✓ Categories label removed from CategoryRow")

    assert_category_row_structure_intact()
    print("✓ CategoryRow structure intact (LazyRow, tiles, filtering, focus)")

    assert_category_tile_styling_preserved()
    print("✓ CategoryTile styling preserved")

    assert_documentation_updated()
    print("✓ Documentation updated (SKILL.md, feature-inventory.md)")

    print("PASS: Categories label removal complete and correct")


if __name__ == "__main__":
    main()

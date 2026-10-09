//! The isometric near-cell geometry the legacy fans neighbour messages over —
//! the second half of T-018 slice 6 (B2).
//!
//! # What the legacy computes
//!
//! `ITZMap::GetCellIndex` (`ITZMap.cpp:116-122`) is a port, verbatim:
//!
//! ```cpp
//! int sx = ( m_mapXtile - y - 1 + x ) / ( m_pWorld->m_cellXtile << 1 );
//! int sy = ( x + y ) / ( m_pWorld->m_cellYtile << 1 );
//! return m_mapXcell * sy + sx;
//! ```
//!
//! and the neighbourhood is the 3×3 block of *grid* coordinates around it
//! (`ITZMap.cpp:71-87`), not the 3×3 block around a flat index — `m_pNearCell[]`
//! is built from `x = i % m_mapXcell; y = i / m_mapXcell` and then `x±1, y±1`.
//! So what a caller actually needs is `sx`, `sy` separately; the flat index is
//! only ever a way to store them.
//!
//! # Why the skew matters
//!
//! The screen is an isometric diamond, so a *rectangular* block of tiles around
//! the player covers off-screen corners and cuts the screen's diagonals short.
//! With plain `x/10, y/15` division the near block was a 30×45 rectangle: an
//! avatar walking toward a screen corner fell out of the block — and so stopped
//! receiving 200s — while still inside the viewport, then vanished abruptly when
//! the client's own screen culling dropped it. The skewed cells follow the
//! diamond instead.
//!
//! # The two sizes
//!
//! `m_cellXtile`/`m_cellYtile` are **not** map data. `CreateWorld` passes
//! `cellXtile = cellYtile = 0` (`MapManager.cpp:82-87`), and 0 selects the
//! screen-in-tiles default (`ITZWorld.h:95-104`):
//!
//! ```text
//! m_screenXtile = (800 + 80 - 1) / 80 = 10
//! m_screenYtile = (600 + 40 - 1) / 40 = 15
//! ```
//!
//! for an 800×600 viewport of 80×40-pixel tiles (`map.h:33-36`). One cell is
//! one screen's worth of tiles, which is why "near" and "on screen" nearly
//! coincide and why getting the skew right is worth doing at all.
//!
//! # Division truncates toward zero
//!
//! The `sx` numerator is `mapXtile - y - 1 + x`, which goes **negative** for
//! tiles near the far corner (`y > mapXtile - 1 + x`), and the legacy's C++ `/`
//! truncates toward zero there — `-3 / 20` is `0`, not `-1`. Rust's `/` on
//! integers truncates toward zero as well, so this module uses `/` and not
//! `div_euclid`. Using `div_euclid` would split one cell in two along the whole
//! far edge of every map.

mod map_geometry_table;

pub use map_geometry_table::MAP_TILE_COUNTS;

/// `m_cellXtile` — see the module docs. Tiles per near-cell on x.
pub const CELL_TILES_X: i32 = 10;

/// `m_cellYtile` — see the module docs. Tiles per near-cell on y.
pub const CELL_TILES_Y: i32 = 15;

/// Tile count used for a `map_id` absent from [`MAP_TILE_COUNTS`].
///
/// The modal value in the extracted table (42 of 452 maps), chosen so an
/// unmapped id still gets plausible diamond geometry instead of a panic. This is
/// a floor on correctness, not a substitute for data: a genuinely new map
/// belongs in the generated table.
const FALLBACK_MAP_TILES: i32 = 100;

/// `m_mapXtile` — the per-map tile width `GetCellIndex` needs.
///
/// `CreateMap` squares the map before storing it (`ITZMap.cpp:39-40`):
///
/// ```cpp
/// if( mapXtile > mapYtile ) mapYtile = mapXtile;
/// if( mapXtile < mapYtile ) mapXtile = mapYtile;
/// ```
///
/// so `m_mapXtile == m_mapYtile` for every map and the width is just the larger
/// of the two raw dimensions. That squaring is why the table stores a single
/// number: map 158 (`ShuriPalace1`) ships as `MapXsize = 45, MapYsize = 105`
/// and is read as **105**, not 45.
#[must_use]
pub fn map_tiles(map_id: i32) -> i32 {
    // The table is sorted by id, so this is a binary search rather than a scan
    // over 452 entries on every tile of every step.
    match MAP_TILE_COUNTS.binary_search_by_key(&map_id, |(id, _)| *id) {
        Ok(idx) => MAP_TILE_COUNTS[idx].1,
        Err(_) => FALLBACK_MAP_TILES,
    }
}

/// The skewed cell a tile falls in, as `(sx, sy)` — the two halves of
/// `GetCellIndex` (`ITZMap.cpp:118-119`), kept separate because the neighbourhood
/// walk compares them one axis at a time.
///
/// A character's `map_id` is required because `sx` subtracts the map's width;
/// the same tile can land in different cells on differently-sized maps.
#[must_use]
pub fn cell_of(x: i32, y: i32, map_id: i32) -> (i32, i32) {
    let width = map_tiles(map_id);
    (
        (width - y - 1 + x) / (CELL_TILES_X << 1),
        (x + y) / (CELL_TILES_Y << 1),
    )
}

/// Whether two cells are within one step on each axis — the `j, k ∈ [-1, 1]`
/// neighbour test of `ITZMap.cpp:71-87`.
#[must_use]
pub fn cells_adjacent(a: (i32, i32), b: (i32, i32)) -> bool {
    (a.0 - b.0).abs() <= 1 && (a.1 - b.1).abs() <= 1
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `CreateWorld(..., 800, 600, MAP_TILE_XSIZE, MAP_TILE_YSIZE)` with the
    /// default cell size (`ITZWorld.h:91-92`, `map.h:33-36`).
    #[test]
    fn cell_size_is_one_screen_of_tiles() {
        assert_eq!(CELL_TILES_X, (800 + 80 - 1) / 80);
        assert_eq!(CELL_TILES_Y, (600 + 40 - 1) / 40);
    }

    /// The two seeded characters both stand on (27, 87) of map 158, which is
    /// inside one another's near cell and is what makes the two-client test work.
    #[test]
    fn the_palace_spawn_shares_one_cell() {
        // (105 - 87 - 1 + 27)/20 == 2, (27 + 87)/30 == 3.
        assert_eq!(cell_of(27, 87, 158), (2, 3));
        // A character a few tiles away lands in the same block, so the two seeded
        // accounts see each other without having to walk first.
        assert!(cells_adjacent(cell_of(27, 87, 158), cell_of(33, 83, 158)));
    }

    /// Map 158 (`ShuriPalace1`) ships `MapXsize = 45, MapYsize = 105` at header
    /// offsets 140/142, and `CreateMap` squares it to 105.
    #[test]
    fn map_tiles_reports_the_squared_header_value() {
        assert_eq!(map_tiles(158), 105);
        assert_eq!(
            map_tiles(158),
            map_tiles(158).max(map_tiles(158)),
            "m_mapXtile == m_mapYtile after CreateMap"
        );
    }

    /// A straight-line reproduction of `ITZMap::GetCellIndex`'s two halves, so the
    /// port is pinned to the C++ rather than to itself.
    #[test]
    fn cell_of_matches_the_cpp_expression() {
        for &(x, y) in &[(0, 0), (27, 87), (45, 44), (46, 45), (104, 104)] {
            let sx = (105 - y - 1 + x) / (10 << 1);
            let sy = (x + y) / (15 << 1);
            assert_eq!(cell_of(x, y, 158), (sx, sy), "tile ({x}, {y})");
        }
    }

    /// C++ `/` truncates toward zero, so the far corner, where
    /// `mapXtile - y - 1 + x` goes negative, belongs to cell 0 and not to cell
    /// -1. `div_euclid` would split that edge in two and open a gap in the
    /// neighbourhood the legacy does not have.
    #[test]
    fn the_far_corner_truncates_toward_zero_not_down() {
        // (0, 104): numerator 105 - 104 - 1 + 0 == 0.
        assert_eq!(cell_of(0, 104, 158), (0, 3));
        // One tile further, y = 105, the numerator is -1 and C++ gives -1/20 == 0.
        assert_eq!(cell_of(0, 105, 158).0, 0);
        assert_ne!(
            cell_of(0, 105, 158).0,
            (-1_i32).div_euclid(20),
            "floor division would put this tile in cell -1"
        );
        // Far enough out the cell really is negative, and still truncated rather
        // than floored: -96/20 is -4 under C++ and -5 under floor division.
        assert_eq!(cell_of(0, 200, 158).0, -4);
        assert_ne!(cell_of(0, 200, 158).0, (-96_i32).div_euclid(20));
    }

    /// The whole point of B2: the cell grid is rotated relative to the tile grid.
    /// A 20-tile step in y moves `sx` by a full cell, which plain `y / 15`
    /// division cannot express, because there `sx` would depend on x alone.
    #[test]
    fn cells_are_skewed_not_a_screen_aligned_rectangle() {
        let base = cell_of(27, 27, 158);
        assert_eq!(base, (5, 1));
        // +20 in x: (105-27-1+47)/20 == 6, (47+27)/30 == 2.
        assert_eq!(cell_of(47, 27, 158), (6, 2));
        // +20 in y: (105-47-1+27)/20 == 4, and the same sy of 2.
        assert_eq!(cell_of(27, 47, 158), (4, 2));
        // Equal steps in x and y give opposite sx. A rectangle keyed on x/10, y/15
        // would have kept sx at 5 for both.
        assert_eq!(cell_of(47, 27, 158).1, cell_of(27, 47, 158).1);
        assert_ne!(cell_of(47, 27, 158).0, cell_of(27, 47, 158).0);
    }

    /// The skew has to actually change who is near: the neighbourhood is one cell
    /// wide along the *cell* axes, and two cells out is outside the 3x3 block.
    /// That is what keeps an avatar fed for as long as it is on screen.
    #[test]
    fn the_neighbourhood_follows_the_diamond() {
        let origin = cell_of(27, 87, 158);
        assert_eq!(origin, (2, 3));
        // One cell along x: (105-87-1+47)/20 == 3, (47+87)/30 == 4.
        assert_eq!(cell_of(47, 87, 158), (3, 4));
        assert!(
            cells_adjacent(origin, cell_of(47, 87, 158)),
            "one cell away is inside the 3x3 block"
        );
        // The other axis, cell (3, 2): diagonal on screen, adjacent on the grid.
        assert_eq!(cell_of(20, 60, 158), (3, 2));
        assert!(cells_adjacent(origin, cell_of(20, 60, 158)));
        // Two cells along x, (105-87-1+67)/20 == 4: outside the block.
        assert_eq!(cell_of(67, 87, 158), (4, 5));
        assert!(
            !cells_adjacent(origin, cell_of(67, 87, 158)),
            "two cells away is outside the 3x3 block"
        );
    }

    /// Map size is part of the cell, so the same tile can be in different
    /// neighbourhoods on different maps and the lookup has to see it.
    #[test]
    fn the_cell_depends_on_the_map_size() {
        assert_eq!(map_tiles(0), 800, "the world map is the outlier");
        assert!(map_tiles(158) < map_tiles(0));
        // The same tile on a wide map and a narrow one.
        assert_ne!(cell_of(27, 87, 0).0, cell_of(27, 87, 158).0);
    }

    #[test]
    fn an_unknown_map_falls_back_rather_than_panicking() {
        // Not in the shipped table.
        assert_eq!(map_tiles(-1), FALLBACK_MAP_TILES);
        assert_eq!(map_tiles(9_999), FALLBACK_MAP_TILES);
        // Still produces a cell, so an unmapped map cannot panic a tile step.
        assert!(cell_of(27, 87, 9_999).1 >= 0);
    }

    /// The table is sorted (the binary search depends on it) and every entry is a
    /// usable size.
    #[test]
    fn the_table_is_sorted_and_sane() {
        assert!(MAP_TILE_COUNTS.len() > 400, "452 shipped map indices");
        for window in MAP_TILE_COUNTS.windows(2) {
            assert!(
                window[0].0 < window[1].0,
                "sorted and unique: {} then {}",
                window[0].0,
                window[1].0
            );
        }
        for &(id, tiles) in MAP_TILE_COUNTS {
            assert!(id >= 0, "map index {id}");
            assert!(tiles > 0, "map {id} has {tiles} tiles");
        }
    }

    #[test]
    fn cells_adjacent_is_one_step_each_axis() {
        assert!(cells_adjacent((5, 5), (5, 5)), "the same cell");
        assert!(cells_adjacent((5, 5), (6, 6)), "one step on both");
        assert!(cells_adjacent((5, 5), (4, 6)), "one step, opposite signs");
        assert!(!cells_adjacent((5, 5), (7, 5)), "two steps on x");
        assert!(!cells_adjacent((5, 5), (5, 7)), "two steps on y");
    }
}

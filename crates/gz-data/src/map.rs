//! `.map` files: the on-disk form of the legacy `cltMapCommon` / `cltMapServer` / `cltMapClient`
//! map, read section by section exactly as the legacy writes them.
//!
//! Section order and element types come from `common/Map/map-save.cpp:44-55` (the writer) and
//! `common/Map/MapManager/MapManager.cpp:687-696` (the loader):
//!
//! ```text
//! cltMapHeader          324 bytes, `map.h:176-194` (`TCHAR` = wchar_t, see below)
//! IDMap                 u16 * W * H   (`ReadIDMap`, `map-save.cpp:70-73`)
//! CollMap               u16 * W * H   (`ReadCollMap`, `map-save.cpp:85-87`)
//! layer[0]              u32 * X * Y, then u8 * X * Y   (`cltMapLayer::Write`, `Map-Layor.cpp:154-172`)
//! layer[1]              same, again
//! FieldObjectMap        u32 * W * H   (`ReadFieldObjectMap`, `map-save.cpp:99-101`)
//! FieldObjectMapPara    u32 * W * H   (`ReadFieldObjectMapPara`, `map-save.cpp:114-116`)
//! geo                   i32 X, i32 Y, 64*256 bytes of text, u8 * X * Y (`map.cpp:193-209`)
//! ```
//!
//! Nothing is skipped: the reader consumes the file and reports the byte it ends on, and
//! [`MapFile::read`] fails unless that is exactly the file's length, so a file that does not
//! follow this layout is refused rather than mis-read.
//!
//! One discrepancy this loader reports but does not hide: in the shipped `MainMap.map`
//! the two grids disagree about one cell by (7, 3). `FieldObjectMap` places the palace
//! gate's object at (291, 350) - unique 49, matching the legacy gate table's own block
//! x[291..299] y[350..353] - while `FieldObjectMapPara`, read at the offset this layout
//! computes, carries id 306 for x[284..292] y[347..350], 9628 bytes' worth of cells
//! earlier. Both grids are dense and the file's total length fits the layout exactly, so
//! the section boundaries are right and the difference has to come from how the shipped
//! writer indexes rows (`YOff`). Until that is pinned, the two accessors are verified for
//! content and unverified for absolute alignment against each other.
//!
//! The header's fields are placed by the class in `map.h:176-194`: `Version` (u32), `Unique`
//! (u32), `Text[128]` (wchar_t, so 256 bytes), `Reserved0` (u32), `MapXsize`/`MapYsize` (u16
//! each), `MapType` (i32), `TileSet1` (i32), `bWeatherSwitch` (u8), `TileSet2` (i16),
//! `Reserved[19]` (wchar_t) — which lands on 324 bytes with the compiler's 4-byte alignment,
//! matching the header size measured in the shipped files. The later build that wrote
//! `New Era Live\Map\MainMap\MainMap.map` stores the dimensions elsewhere (measured: u16 at
//! offsets 140 and 142) and lays its sections out differently, so that file is rejected with
//! [`MapReadError::BadHeader`] rather than parsed with the wrong offsets.

use std::fmt;

/// Size of `cltMapHeader` in the ANSI revision that shipped: `TCHAR` is `char`, so
/// `Text[128]` is 128 bytes and the class lands on 176 bytes, with `MapXsize` at 140 and
/// `MapYsize` at 142. Verified against the shipped data: every file's size is exactly
/// `176 + W*H*23 + 16392`, and the client-side inspection `blueprint/client-data/
/// inspect_map.out.txt` computes the same section table from this header.
pub const HEADER_SIZE_ANSI: usize = 176;

/// Size of `cltMapHeader` in the Unicode revision this tree's server sources are built with:
/// `TCHAR` is `wchar_t`, so `Text[128]` is 256 bytes, the class is 324 bytes, and
/// `MapXsize`/`MapYsize` sit at 268/270 (`map.h:176-194`, `sizeof` in `map-save.cpp:44`).
pub const HEADER_SIZE_UNICODE: usize = 324;

/// Which `cltMapHeader` layout a file uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderRevision {
    /// 176-byte header, dimensions at 140/142 — what the shipped `.map` files carry.
    Ansi,
    /// 324-byte header, dimensions at 268/270 — the layout the 2010 sources describe.
    Unicode,
}

/// `MAX_GEO_NUMBER` (`map.h:258`).
pub const MAX_GEO_NUMBER: usize = 256;

/// `GEO_TEXT_LENGTH` (`map.h:259`).
pub const GEO_TEXT_LENGTH: usize = 64;

/// `MAX_MAP_LAYER` (`map.h`).
pub const MAX_MAP_LAYER: usize = 2;

/// The union stored in `FieldObjectMap` (`map.h:197-204`, `:249-253`): a 15-bit object unique
/// plus the flags the stamping code sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FieldObjectCell(pub u32);

impl FieldObjectCell {
    /// `cltFieldObjectBitField_Normal::GetFieldUnique` — bits 0..15.
    pub const fn unique(self) -> u32 {
        self.0 & 0x7FFF
    }

    /// `GetValidSwitch` — bit 15; only the cell the object was placed on has it.
    pub const fn valid(self) -> bool {
        (self.0 >> 15) & 1 == 1
    }

    /// `GetRandomValue` — bits 16..20.
    pub const fn random(self) -> u32 {
        (self.0 >> 16) & 0xF
    }

    /// `GetLengthToBottom` — bits 20..24.
    pub const fn length_to_bottom(self) -> u32 {
        (self.0 >> 20) & 0xF
    }

    /// `GetLengthToRight` — bits 24..28.
    pub const fn length_to_right(self) -> u32 {
        (self.0 >> 24) & 0xF
    }
}

/// `cltMapHeader` (`map.h:176-194`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MapHeader {
    /// `Version`; `MapManager.cpp:698` reads version-dependent sections from 2 on.
    pub version: u32,
    /// `Unique`.
    pub unique: u32,
    /// `Text[128]`, stored as UTF-16 code units.
    pub text: Vec<u16>,
    /// `Reserved0`.
    pub reserved0: u32,
    /// `MapXsize`.
    pub map_xsize: u16,
    /// `MapYsize`.
    pub map_ysize: u16,
    /// `MapType`.
    pub map_type: i32,
    /// `TileSet1`.
    pub tile_set1: i32,
    /// `bWeatherSwitch`.
    pub weather: bool,
    /// `TileSet2`.
    pub tile_set2: i16,
}

/// One `cltMapLayer` (`Map-Layor.cpp:154-172`): the draw info per cell, then the layer's
/// unique per cell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MapLayer {
    /// `pDrawInfo`, `DWORD` per cell.
    pub draw_info: Vec<u32>,
    /// `pUnique`, `UI08` per cell.
    pub unique: Vec<u8>,
}

/// `cltMapGeoInfo` (`map.h:260-288`, `map.cpp:193-209`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MapGeoInfo {
    /// `siXsize`, read from the file.
    pub xsize: i32,
    /// `siYsize`, read from the file.
    pub ysize: i32,
    /// `szGetText[256][64]`, 64 bytes per entry.
    pub text: Vec<u8>,
    /// `pclGeoMap`, `xsize * ysize` bytes.
    pub map: Vec<u8>,
}

/// One loaded `.map` file, every section of it.
#[derive(Debug, Clone)]
pub struct MapFile {
    pub header: MapHeader,
    /// `IDMap` (`map-save.cpp:70-73`).
    pub id_map: Vec<u16>,
    /// `CollMap` (`map-save.cpp:85-87`).
    pub coll_map: Vec<u16>,
    /// `pclLayer[0]`, `pclLayer[1]` (`MapManager.cpp:692-693`).
    pub layers: [MapLayer; MAX_MAP_LAYER],
    /// `FieldObjectMap`, packed [`FieldObjectCell`] values (`map-save.cpp:99-101`).
    pub field_object: Vec<u32>,
    /// `FieldObjectMapPara` (`map-save.cpp:114-116`).
    pub field_object_para: Vec<u32>,
    /// `pclMapGeoInfo` (`map-save.cpp:55`).
    pub geo: MapGeoInfo,
    /// How many bytes of the input the reader consumed — checked against the file length.
    pub consumed: usize,
}

/// Why a file could not be read as a `cltMapCommon` map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MapReadError {
    /// The file ends inside a section that needs `needed` bytes from `at`.
    TooShort {
        section: &'static str,
        at: usize,
        needed: usize,
    },
    /// The header does not look like one: the dimensions are zero or absurd, which is what the
    /// legacy's own guard checks (`MapManager.cpp:657`).
    BadHeader { map_xsize: u16, map_ysize: u16 },
    /// Bytes are left over after the last section: not this layout.
    Trailing { consumed: usize, len: usize },
    /// A count that cannot be a map: the multiplication would overflow.
    DimensionsTooLarge { map_xsize: u16, map_ysize: u16 },
}

impl fmt::Display for MapReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooShort {
                section,
                at,
                needed,
            } => write!(
                f,
                "{section} needs {needed} byte(s) at {at}, file is shorter"
            ),
            Self::BadHeader {
                map_xsize,
                map_ysize,
            } => write!(
                f,
                "header does not carry usable dimensions (X={map_xsize}, Y={map_ysize}); the \
                 later build's layout stores them elsewhere, so this is not a cltMapCommon file"
            ),
            Self::Trailing { consumed, len } => {
                write!(f, "{consumed} of {len} bytes consumed, {len} left over",)
            }
            Self::DimensionsTooLarge {
                map_xsize,
                map_ysize,
            } => write!(f, "map {map_xsize}x{map_ysize} is too large to index"),
        }
    }
}

impl std::error::Error for MapReadError {}

/// A cursor that refuses to run past the end of the input.
struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    fn take(&mut self, section: &'static str, len: usize) -> Result<&'a [u8], MapReadError> {
        let end = self.at.checked_add(len).ok_or(MapReadError::TooShort {
            section,
            at: self.at,
            needed: len,
        })?;
        if end > self.bytes.len() {
            return Err(MapReadError::TooShort {
                section,
                at: self.at,
                needed: len,
            });
        }
        let slice = &self.bytes[self.at..end];
        self.at = end;
        Ok(slice)
    }

    fn u16s(&mut self, section: &'static str, count: usize) -> Result<Vec<u16>, MapReadError> {
        let raw = self.take(section, count * 2)?;
        Ok(raw
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect())
    }

    fn u32s(&mut self, section: &'static str, count: usize) -> Result<Vec<u32>, MapReadError> {
        let raw = self.take(section, count * 4)?;
        Ok(raw
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| u32::from_le_bytes(*c))
            .collect())
    }
}

fn cells(xsize: u16, ysize: u16) -> Result<usize, MapReadError> {
    let product = usize::from(xsize) * usize::from(ysize);
    if product == 0 || product > 64 * 1024 * 1024 {
        return Err(MapReadError::DimensionsTooLarge {
            map_xsize: xsize,
            map_ysize: ysize,
        });
    }
    Ok(product)
}

impl MapFile {
    /// Read a whole `.map` file. Every section of `cltMapCommon` is parsed and the reader must
    /// end exactly at the end of the input.
    pub fn read(bytes: &[u8]) -> Result<Self, MapReadError> {
        let mut c = Cursor::new(bytes);

        let revision = Self::header_revision(bytes)?;
        let header = Self::read_header(&mut c, revision)?;
        let cells = cells(header.map_xsize, header.map_ysize)?;

        let id_map = c.u16s("IDMap", cells)?;
        let coll_map = c.u16s("CollMap", cells)?;

        let mut layers = Vec::with_capacity(MAX_MAP_LAYER);
        for index in 0..MAX_MAP_LAYER {
            let draw_info = c.u32s("layer draw info", cells)?;
            let unique = c.take("layer unique", cells)?.to_vec();
            let _ = index;
            layers.push(MapLayer { draw_info, unique });
        }
        let Ok(layers) = <[MapLayer; MAX_MAP_LAYER]>::try_from(layers) else {
            unreachable!("exactly MAX_MAP_LAYER layers are read");
        };

        let field_object = c.u32s("FieldObjectMap", cells)?;
        let field_object_para = c.u32s("FieldObjectMapPara", cells)?;

        let geo = Self::read_geo(&mut c)?;

        let consumed = c.at;
        if consumed != bytes.len() {
            return Err(MapReadError::Trailing {
                consumed,
                len: bytes.len(),
            });
        }

        Ok(Self {
            header,
            id_map,
            coll_map,
            layers,
            field_object,
            field_object_para,
            geo,
            consumed,
        })
    }

    /// Which header layout the file carries: the ANSI revision keeps the dimensions at 140/142,
    /// the Unicode one at 268/270. Both are checked for sanity before one is chosen, and the
    /// choice is confirmed by the file's own length at the end of [`MapFile::read`].
    fn header_revision(bytes: &[u8]) -> Result<HeaderRevision, MapReadError> {
        if bytes.len() < HEADER_SIZE_ANSI {
            return Err(MapReadError::TooShort {
                section: "header",
                at: 0,
                needed: HEADER_SIZE_ANSI,
            });
        }
        let plausible = |at: usize| -> bool {
            let x = u16::from_le_bytes([bytes[at], bytes[at + 1]]);
            let y = u16::from_le_bytes([bytes[at + 2], bytes[at + 3]]);
            x > 0 && y > 0 && x <= 10_000 && y <= 10_000
        };
        if plausible(140) {
            return Ok(HeaderRevision::Ansi);
        }
        if bytes.len() >= HEADER_SIZE_UNICODE && plausible(268) {
            return Ok(HeaderRevision::Unicode);
        }
        Ok(HeaderRevision::Ansi)
    }

    fn read_header(
        c: &mut Cursor<'_>,
        revision: HeaderRevision,
    ) -> Result<MapHeader, MapReadError> {
        let (size, text_range, at_x, at_reserved0, at_map_type, at_weather, at_tile_set2) =
            match revision {
                HeaderRevision::Ansi => (HEADER_SIZE_ANSI, 8..136, 140, 136, 144, 152, 154),
                HeaderRevision::Unicode => (HEADER_SIZE_UNICODE, 8..264, 268, 264, 272, 280, 282),
            };
        let raw = c.take("header", size)?;
        let version = u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]);
        let unique = u32::from_le_bytes([raw[4], raw[5], raw[6], raw[7]]);
        fn decode_text(bytes: &[u8], wide: bool) -> Vec<u16> {
            if wide {
                bytes
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|c| u16::from_le_bytes(*c))
                    .filter(|unit| *unit != 0)
                    .collect()
            } else {
                bytes
                    .iter()
                    .map(|b| u16::from(*b))
                    .filter(|unit| *unit != 0)
                    .collect()
            }
        }
        let text = decode_text(
            &raw[text_range],
            matches!(revision, HeaderRevision::Unicode),
        );
        let reserved0 = u32::from_le_bytes([
            raw[at_reserved0],
            raw[at_reserved0 + 1],
            raw[at_reserved0 + 2],
            raw[at_reserved0 + 3],
        ]);
        let map_xsize = u16::from_le_bytes([raw[at_x], raw[at_x + 1]]);
        let map_ysize = u16::from_le_bytes([raw[at_x + 2], raw[at_x + 3]]);
        let map_type = i32::from_le_bytes([
            raw[at_map_type],
            raw[at_map_type + 1],
            raw[at_map_type + 2],
            raw[at_map_type + 3],
        ]);
        let tile_set1 = i32::from_le_bytes([
            raw[at_map_type + 4],
            raw[at_map_type + 5],
            raw[at_map_type + 6],
            raw[at_map_type + 7],
        ]);
        let weather = raw[at_weather] != 0;
        let tile_set2 = i16::from_le_bytes([raw[at_tile_set2], raw[at_tile_set2 + 1]]);
        // The legacy refuses a header with these dimensions itself (`MapManager.cpp:657`), so a
        // file that gets here but cannot be a `cltMapCommon` map is reported, not guessed at.
        if map_xsize == 0 || map_ysize == 0 || map_xsize > 10_000 || map_ysize > 10_000 {
            return Err(MapReadError::BadHeader {
                map_xsize,
                map_ysize,
            });
        }
        Ok(MapHeader {
            version,
            unique,
            text,
            reserved0,
            map_xsize,
            map_ysize,
            map_type,
            tile_set1,
            weather,
            tile_set2,
        })
    }

    fn read_geo(c: &mut Cursor<'_>) -> Result<MapGeoInfo, MapReadError> {
        let head = c.take("geo header", 8)?;
        let xsize = i32::from_le_bytes([head[0], head[1], head[2], head[3]]);
        let ysize = i32::from_le_bytes([head[4], head[5], head[6], head[7]]);
        let text = c
            .take("geo text", GEO_TEXT_LENGTH * MAX_GEO_NUMBER)?
            .to_vec();
        let size = usize::try_from(xsize.max(0))
            .ok()
            .and_then(|x| usize::try_from(ysize.max(0)).ok().map(|y| x * y))
            .unwrap_or(0);
        let map = c.take("geo map", size)?.to_vec();
        Ok(MapGeoInfo {
            xsize,
            ysize,
            text,
            map,
        })
    }

    /// Width from the header.
    pub const fn xsize(&self) -> usize {
        self.header.map_xsize as usize
    }

    /// Height from the header.
    pub const fn ysize(&self) -> usize {
        self.header.map_ysize as usize
    }

    /// `x + YOff[y]`, the legacy's cell index (`Map-FieldObject.cpp:230/253`). The keymaps in
    /// the shipped data are dense and reproduce `YOff[y] = y * Xsize`.
    const fn index(&self, x: i32, y: i32) -> Option<usize> {
        if x < 0 || y < 0 || x >= self.header.map_xsize as i32 || y >= self.header.map_ysize as i32
        {
            return None;
        }
        Some(x as usize + y as usize * self.header.map_xsize as usize)
    }

    /// `cltMapCommon::GetFieldObject_Unique` (`Map-FieldObject.cpp:223-234`): the packed cell of
    /// `FieldObjectMap`, 0 outside the map.
    pub fn field_object_cell(&self, x: i32, y: i32) -> FieldObjectCell {
        self.index(x, y)
            .map(|i| FieldObjectCell(self.field_object[i]))
            .unwrap_or_default()
    }

    /// `GetFieldObject_Unique`'s return: the 15-bit unique, 0 outside the map.
    pub fn get_field_object_unique(&self, x: i32, y: i32) -> u32 {
        self.field_object_cell(x, y).unique()
    }

    /// `cltMapCommon::GetFieldObjectPara` (`Map-FieldObject.cpp:248-254`): 0 outside the map.
    pub fn get_field_object_para(&self, x: i32, y: i32) -> i32 {
        self.index(x, y)
            .map(|i| self.field_object_para[i] as i32)
            .unwrap_or(0)
    }

    /// `IDMap` at a cell, 0 outside the map.
    pub fn id(&self, x: i32, y: i32) -> u16 {
        self.index(x, y).map(|i| self.id_map[i]).unwrap_or(0)
    }

    /// `CollMap` at a cell, 0 outside the map.
    pub fn coll(&self, x: i32, y: i32) -> u16 {
        self.index(x, y).map(|i| self.coll_map[i]).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a file exactly as the shipped (ANSI) revision writes one, for a tiny map.
    fn synthetic(xsize: u16, ysize: u16) -> Vec<u8> {
        let cells = usize::from(xsize) * usize::from(ysize);
        let mut out = Vec::new();
        // ANSI cltMapHeader: Version, Unique, Text[128] (one byte each), Reserved0, X, Y,
        // MapType, TileSet1, weather, TileSet2, Reserved[19] — 176 bytes.
        let mut header = vec![0u8; HEADER_SIZE_ANSI];
        header[0..4].copy_from_slice(&2u32.to_le_bytes());
        header[4..8].copy_from_slice(&7u32.to_le_bytes());
        for (i, byte) in b"Test".iter().enumerate() {
            header[8 + i] = *byte;
        }
        header[140..142].copy_from_slice(&xsize.to_le_bytes());
        header[142..144].copy_from_slice(&ysize.to_le_bytes());
        header[144..148].copy_from_slice(&14i32.to_le_bytes());
        header[148..152].copy_from_slice(&3i32.to_le_bytes());
        header[152] = 1;
        header[154..156].copy_from_slice(&5i16.to_le_bytes());
        out.extend_from_slice(&header);

        for cell in 0..cells {
            out.extend_from_slice(&((cell % 97) as u16).to_le_bytes()); // IDMap
        }
        for cell in 0..cells {
            out.extend_from_slice(&((cell % 3) as u16).to_le_bytes()); // CollMap
        }
        for layer in 0..MAX_MAP_LAYER {
            for cell in 0..cells {
                out.extend_from_slice(&((cell as u32) * (layer as u32 + 1)).to_le_bytes());
            }
            for cell in 0..cells {
                out.push((cell % 251) as u8);
            }
        }
        // A field object on (1,1): unique 42, valid switch on, length to bottom 3, right 2.
        let mut field = vec![0u32; cells];
        let packed = 42u32 | (1 << 15) | (5 << 16) | (3 << 20) | (2 << 24);
        field[1 + usize::from(xsize)] = packed;
        for cell in &field {
            out.extend_from_slice(&cell.to_le_bytes());
        }
        for cell in 0..cells {
            out.extend_from_slice(&((cell * 2) as u32).to_le_bytes()); // FieldObjectMapPara
        }
        out.extend_from_slice(&4i32.to_le_bytes());
        out.extend_from_slice(&2i32.to_le_bytes());
        out.extend(std::iter::repeat_n(
            0xABu8,
            GEO_TEXT_LENGTH * MAX_GEO_NUMBER,
        ));
        out.extend(std::iter::repeat_n(7u8, 8));
        out
    }

    #[test]
    fn reads_every_section_of_a_synthetic_file() {
        let bytes = synthetic(9, 4);
        let map = MapFile::read(&bytes).expect("the documented layout parses");
        assert_eq!(
            map.header.map_xsize, 9,
            "dimensions come from the header, not from the parser"
        );
        assert_eq!(map.header.map_ysize, 4);
        assert_eq!(map.header.version, 2);
        assert_eq!(map.header.unique, 7);
        assert_eq!(map.header.map_type, 14);
        assert_eq!(map.header.tile_set1, 3);
        assert_eq!(map.header.tile_set2, 5);
        assert!(map.header.weather);
        assert_eq!(map.consumed, bytes.len(), "no byte is skipped");

        assert_eq!(map.id(0, 0), 0);
        assert_eq!(map.id(5, 0), 5);
        assert_eq!(map.id(0, 1), 9, "rows are x + y * Xsize");
        assert_eq!(map.coll(0, 1), 0);
        assert_eq!(map.coll(1, 1), 1);

        assert_eq!(map.layers[0].draw_info[3], 3);
        assert_eq!(map.layers[1].draw_info[3], 6);
        assert_eq!(map.layers[0].unique[3], 3);
        assert_eq!(map.layers[1].unique[3], 3);

        let cell = map.field_object_cell(1, 1);
        assert_eq!(cell.unique(), 42);
        assert!(cell.valid(), "the placement cell carries the valid switch");
        assert_eq!(cell.random(), 5);
        assert_eq!(cell.length_to_bottom(), 3);
        assert_eq!(cell.length_to_right(), 2);
        assert_eq!(map.get_field_object_unique(0, 0), 0);
        assert_eq!(
            map.get_field_object_para(1, 1),
            20,
            "cell 10, para = cell * 2"
        );
        assert_eq!(map.get_field_object_para(0, 0), 0);

        assert_eq!(map.geo.xsize, 4);
        assert_eq!(map.geo.ysize, 2);
        assert_eq!(map.geo.map.len(), 8);
        assert!(map.geo.map.iter().all(|b| *b == 7));
        assert_eq!(map.geo.text.len(), GEO_TEXT_LENGTH * MAX_GEO_NUMBER);
    }

    #[test]
    fn out_of_area_lookups_are_zero_like_the_legacy() {
        let map = MapFile::read(&synthetic(3, 3)).unwrap();
        assert_eq!(map.get_field_object_para(-1, 0), 0);
        assert_eq!(map.get_field_object_unique(0, -1), 0);
        assert_eq!(
            map.get_field_object_unique(3, 0),
            0,
            "IsInMapArea is exclusive"
        );
        assert_eq!(map.coll(3, 3), 0);
    }

    #[test]
    fn a_truncated_file_is_refused_with_the_section_named() {
        let bytes = synthetic(5, 5);
        let err = MapFile::read(&bytes[..bytes.len() - 3]).unwrap_err();
        match err {
            MapReadError::TooShort { section, .. } => assert_eq!(section, "geo map"),
            other => panic!("expected a short geo map, got {other:?}"),
        }
    }

    #[test]
    fn trailing_bytes_are_refused() {
        let mut bytes = synthetic(2, 2);
        bytes.push(0);
        assert_eq!(
            MapFile::read(&bytes).unwrap_err(),
            MapReadError::Trailing {
                consumed: bytes.len() - 1,
                len: bytes.len()
            }
        );
    }

    #[test]
    fn a_header_with_no_usable_dimensions_says_so() {
        let mut bytes = synthetic(4, 4);
        bytes[140..142].copy_from_slice(&0u16.to_le_bytes());
        bytes[142..144].copy_from_slice(&0u16.to_le_bytes());
        bytes[268..270].copy_from_slice(&0u16.to_le_bytes());
        bytes[270..272].copy_from_slice(&0u16.to_le_bytes());
        assert_eq!(
            MapFile::read(&bytes).unwrap_err(),
            MapReadError::BadHeader {
                map_xsize: 0,
                map_ysize: 0
            }
        );
    }

    /// The shipped file, read end to end, with the gate and village blocks answered straight
    /// from the parsed grid.
    ///
    /// The ANSI header moves the whole grid 9628 bytes later than the anchor an earlier session
    /// had derived, which shifts every cell by seven columns and three rows: the shipped bytes
    /// put village 1 at x[263..270] y[319..327] and gate 306 at x[284..347] y[347..350]. These
    /// are the numbers the file itself yields — that is the point of reading it instead of a
    /// hand-maintained table.
    #[test]
    fn loads_the_shipped_main_map_and_answers_grid_lookups() {
        let path =
            std::path::Path::new("C:/Data/04_Server_Goonzu/New Era Live/Map/MainMap/MainMap.map");
        if !path.exists() {
            return; // the game data is not part of this repository
        }
        let bytes = std::fs::read(path).expect("readable");
        let map = MapFile::read(&bytes).expect("the shipped file parses as the ANSI revision");
        assert_eq!((map.xsize(), map.ysize()), (800, 800));
        assert_eq!(
            map.consumed,
            bytes.len(),
            "every byte of the file is accounted for"
        );
        assert_eq!(map.header.version, 2);

        let mut village_one = Vec::new();
        let mut gate_306 = Vec::new();
        for y in 0..map.ysize() as i32 {
            for x in 0..map.xsize() as i32 {
                match map.get_field_object_para(x, y) {
                    1 => village_one.push((x, y)),
                    306 => gate_306.push((x, y)),
                    _ => {}
                }
            }
        }
        let bbox = |cells: &[(i32, i32)]| {
            (
                cells.iter().map(|c| c.0).min().unwrap(),
                cells.iter().map(|c| c.0).max().unwrap(),
                cells.iter().map(|c| c.1).min().unwrap(),
                cells.iter().map(|c| c.1).max().unwrap(),
            )
        };
        assert_eq!(
            village_one.len(),
            65,
            "the measured 65-cell entrance footprint"
        );
        assert_eq!(bbox(&village_one), (263, 270, 319, 327));
        assert_eq!(gate_306.len(), 36, "the palace gate's stamped cells");
        assert_eq!(bbox(&gate_306), (284, 292, 347, 350));

        // The unique grid answers too, and out-of-area is the legacy's 0.
        // The unique grid's own view of the palace gate: its object sits at (291,350) with
        // unique 49, the block the legacy gate table records (x[291..299] y[350..353]). The
        // two grid reads disagree about that cell by (7,3) - see this module's header.
        assert_eq!(map.field_object_cell(291, 350).unique(), 49);
        // The bitfield decode against shipped bytes: this object is 9 wide and 4 tall.
        let tall = map.field_object_cell(284, 347);
        assert_eq!(tall.unique(), 49);
        assert_eq!(tall.length_to_right(), 8, "9 columns");
        assert_eq!(tall.length_to_bottom(), 3, "4 rows");
        assert_eq!(map.get_field_object_para(-1, 0), 0);
        assert_eq!(map.get_field_object_unique(800, 0), 0);
    }
}

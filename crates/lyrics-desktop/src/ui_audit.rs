#![allow(dead_code)]
//! Pixel-level assertions for the headless UI snapshot harness (`ui_snapshot`).
//!
//! The snapshot test renders the real Slint window into an in-memory buffer.
//! This module turns that buffer into a list of *failures*, so a theming or
//! layout regression breaks `cargo test` — and therefore CI — instead of
//! quietly shipping.
//!
//! Two families of checks live here:
//!
//! * [`Frame::unthemed_blocks`] finds large uniform light-grey areas, i.e. a
//!   platform-themed widget (`LineEdit`, `Slider`, `CheckBox`, …) that bypassed
//!   the app theme. The design is near-black, so any big light-grey rectangle is
//!   a bug. Thin white *text* is deliberately not flagged: glyphs are small and
//!   antialiased, so they never form a large uniform block.
//! * Islands/bands/centroids, used by `ui_snapshot` to assert layout geometry on
//!   fixed-size, font-independent elements (layout boxes, the active-tab
//!   indicator, SVG icons). Text is measured by centroid where possible so the
//!   assertions hold across the different font stacks of the CI runners.

use slint::platform::software_renderer::PremultipliedRgbaColor;
use std::collections::HashMap;

/// Luma at or above this counts as "ink", i.e. intentionally painted content.
/// Low enough to catch `Theme.text-dim` (luma ~85), high enough to ignore the
/// hover/elevated surfaces (luma <= ~36).
pub const INK_LUMA: u32 = 45;
/// Ink pixels a row needs before it counts as painted (suppresses stray pixels).
const ROW_MIN_PIXELS: usize = 3;
/// Deliberately dim content, e.g. the unplayed portion of the seek waveform,
/// which is darker than `Theme.text-dim` but still clearly painted.
/// Dim content that still reads as painted foreground. Every surface in the app is
/// darker than this — the page background sits at luma 16, the header at 26 and the
/// list cards at 34 — so a threshold below those turns whole backgrounds into one
/// "island" and every geometry check that measures edges silently measures the
/// surface instead. The dimmest foreground the audits look for is the unplayed part
/// of the waveform, at luma 46.
pub const FAINT_LUMA: u32 = 40;
/// Near-white content. On this theme that is reserved for emphasis — the active
/// lyric line, for instance — so it is how the lyrics panel's current line is
/// located without depending on a highlight box behind it.
pub const BRIGHT_LUMA: u32 = 200;
/// Horizontal gap that separates two painted elements from one another.
pub const ISLAND_GAP: usize = 16;

// Unthemed-widget detection. These mirror the thresholds of the ad-hoc Python
// audit this module replaces, which were calibrated against the app's palette.
const LIGHT_LUMA_LO: u32 = 140;
const LIGHT_LUMA_HI: u32 = 253;
const LIGHT_MAX_SATURATION: u32 = 26;
const MIN_BLOCK_W: usize = 24;
const MIN_BLOCK_H: usize = 12;
const MIN_UNIFORMITY: f64 = 0.75;

/// A horizontally contiguous painted element.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Island {
    pub x0: usize,
    pub x1: usize,
    pub y0: usize,
    pub y1: usize,
    pub pixels: usize,
}

impl Island {
    pub fn centre_x(&self) -> f64 {
        (self.x0 + self.x1) as f64 / 2.0
    }

    #[allow(dead_code)]
    pub fn centre_y(&self) -> f64 {
        (self.y0 + self.y1) as f64 / 2.0
    }

    pub fn height(&self) -> usize {
        self.y1.saturating_sub(self.y0) + 1
    }

    pub fn width(&self) -> usize {
        self.x1.saturating_sub(self.x0) + 1
    }

    pub fn to_rect(self) -> Rect {
        Rect::new(self.x0, self.x1 + 1, self.y0, self.y1 + 1)
    }
}

/// An axis-aligned rectangular region for layout and container boundary checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x0: usize,
    pub x1: usize,
    pub y0: usize,
    pub y1: usize,
}

impl Rect {
    pub const fn new(x0: usize, x1: usize, y0: usize, y1: usize) -> Self {
        Self { x0, x1, y0, y1 }
    }

    pub fn width(&self) -> usize {
        self.x1.saturating_sub(self.x0)
    }

    pub fn height(&self) -> usize {
        self.y1.saturating_sub(self.y0)
    }

    pub fn intersects(&self, other: &Self) -> bool {
        self.x0 < other.x1 && self.x1 > other.x0 && self.y0 < other.y1 && self.y1 > other.y0
    }
}

/// A large uniform light region: the signature of an OS-themed widget.
#[derive(Clone, Copy, Debug)]
pub struct Block {
    pub x0: usize,
    pub x1: usize,
    pub y0: usize,
    pub y1: usize,
    pub pixels: usize,
    pub colour: [u8; 3],
}

impl std::fmt::Display for Block {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "x{}..{} y{}..{} ({}x{}, {}px, rgb{:?})",
            self.x0,
            self.x1,
            self.y0,
            self.y1,
            self.width(),
            self.height(),
            self.pixels,
            self.colour
        )
    }
}

impl Block {
    pub fn height(&self) -> usize {
        self.y1 - self.y0 + 1
    }

    pub fn width(&self) -> usize {
        self.x1 - self.x0 + 1
    }
}

/// One rendered window, kept in memory after it has been written to disk.
pub struct Frame {
    pixels: Vec<[u8; 3]>,
    width: usize,
    height: usize,
}

impl Frame {
    /// Copies a Slint software-renderer buffer into an analysable frame.
    pub fn new(buffer: &[PremultipliedRgbaColor], width: usize, height: usize) -> Self {
        assert_eq!(
            buffer.len(),
            width * height,
            "software renderer buffer does not match the frame size"
        );
        Self {
            pixels: buffer.iter().map(|p| [p.red, p.green, p.blue]).collect(),
            width,
            height,
        }
    }

    #[allow(dead_code)]
    pub fn width(&self) -> usize {
        self.width
    }

    #[allow(dead_code)]
    pub fn height(&self) -> usize {
        self.height
    }

    pub fn pixel(&self, x: usize, y: usize) -> [u8; 3] {
        self.pixels[y * self.width + x]
    }

    /// Rec. 601-ish luma; the exact weights do not matter, only the ordering.
    pub fn luma(&self, x: usize, y: usize) -> u32 {
        let [r, g, b] = self.pixel(x, y);
        (r as u32 * 30 + g as u32 * 59 + b as u32 * 11) / 100
    }

    fn ink(&self, x: usize, y: usize) -> bool {
        self.luma(x, y) >= INK_LUMA
    }

    fn is_light_grey(&self, x: usize, y: usize) -> bool {
        let [r, g, b] = self.pixel(x, y);
        let luma = self.luma(x, y);
        let saturation = r.max(g).max(b) - r.min(g).min(b);
        (LIGHT_LUMA_LO..=LIGHT_LUMA_HI).contains(&luma)
            && (saturation as u32) <= LIGHT_MAX_SATURATION
    }

    /// The app's violet accent (`Theme.accent-violet` and friends).
    fn is_violet(&self, x: usize, y: usize) -> bool {
        let [r, g, b] = self.pixel(x, y);
        let (r, g, b) = (r as i32, g as i32, b as i32);
        b > 110 && b - g > 40 && b - r > 20
    }

    /// Rows in `y0..y1` that contain at least [`ROW_MIN_PIXELS`] ink pixels.
    fn painted_rows(&self, x0: usize, x1: usize, y0: usize, y1: usize) -> Vec<usize> {
        (y0..y1.min(self.height))
            .filter(|&y| {
                (x0..x1.min(self.width)).filter(|&x| self.ink(x, y)).count() >= ROW_MIN_PIXELS
            })
            .collect()
    }

    /// Every painted band in a region, top to bottom. Elements are spaced by
    /// >= 10px, so a short blank gap stays inside one band.
    pub fn ink_bands(&self, x0: usize, x1: usize, y0: usize, y1: usize) -> Vec<(usize, usize)> {
        let rows = self.painted_rows(x0, x1, y0, y1);
        let mut bands: Vec<(usize, usize)> = Vec::new();
        for row in rows {
            match bands.last_mut() {
                Some((_, last)) if row - *last <= 6 => *last = row,
                _ => bands.push((row, row)),
            }
        }
        bands
    }

    /// The topmost painted element in a region. Used to isolate the icon that
    /// heads an empty state.
    pub fn first_ink_band(
        &self,
        x0: usize,
        x1: usize,
        y0: usize,
        y1: usize,
    ) -> Option<(usize, usize)> {
        self.ink_bands(x0, x1, y0, y1).into_iter().next()
    }

    /// Vertical extent of the painted content in a region.
    pub fn ink_extent_y(
        &self,
        x0: usize,
        x1: usize,
        y0: usize,
        y1: usize,
    ) -> Option<(usize, usize)> {
        let rows = self.painted_rows(x0, x1, y0, y1);
        Some((*rows.first()?, *rows.last()?))
    }

    /// Horizontal extent of the painted content in a region.
    pub fn ink_extent_x(
        &self,
        x0: usize,
        x1: usize,
        y0: usize,
        y1: usize,
    ) -> Option<(usize, usize)> {
        let cols = self.matching_columns(x0, x1, y0, y1, |x, y| self.ink(x, y));
        Some((*cols.first()? as usize, *cols.last()? as usize))
    }

    /// Columns in `y0..y1` containing a pixel matching `predicate`.
    fn matching_columns(
        &self,
        x0: usize,
        x1: usize,
        y0: usize,
        y1: usize,
        predicate: impl Fn(usize, usize) -> bool,
    ) -> Vec<isize> {
        (x0..x1.min(self.width))
            .filter(|&x| (y0..y1.min(self.height)).any(|y| predicate(x, y)))
            .map(|x| x as isize)
            .collect()
    }

    fn group_columns(
        &self,
        columns: &[isize],
        y0: usize,
        y1: usize,
        gap: usize,
        predicate: impl Fn(usize, usize) -> bool,
    ) -> Vec<Island> {
        let mut islands = Vec::new();
        let mut start: Option<usize> = None;
        let mut previous = 0usize;

        for (index, &column) in columns.iter().enumerate() {
            let column = column as usize;
            match start {
                None => {
                    start = Some(column);
                    previous = column;
                }
                Some(_) if column - previous > gap => {
                    islands.push(self.island(start.unwrap(), previous, y0, y1, &predicate));
                    start = Some(column);
                    previous = column;
                }
                Some(_) => previous = column,
            }
            if index == columns.len() - 1 {
                islands.push(self.island(start.unwrap(), previous, y0, y1, &predicate));
            }
        }
        islands
    }

    fn island(
        &self,
        x0: usize,
        x1: usize,
        y0: usize,
        y1: usize,
        predicate: &impl Fn(usize, usize) -> bool,
    ) -> Island {
        let mut min_y = usize::MAX;
        let mut max_y = 0;
        let mut pixels = 0;
        for x in x0..=x1 {
            for y in y0..y1.min(self.height) {
                if predicate(x, y) {
                    min_y = min_y.min(y);
                    max_y = max_y.max(y);
                    pixels += 1;
                }
            }
        }
        Island {
            x0,
            x1,
            y0: min_y,
            y1: max_y,
            pixels,
        }
    }

    /// Painted elements along one horizontal band. `gap` is the widest run of
    /// blank columns that still belongs to one element: a large gap groups a
    /// whole control together, a small one splits an icon from its label.
    pub fn islands(
        &self,
        x0: usize,
        x1: usize,
        y0: usize,
        y1: usize,
        min_luma: u32,
        gap: usize,
    ) -> Vec<Island> {
        let above = |x: usize, y: usize| self.luma(x, y) >= min_luma;
        let columns = self.matching_columns(x0, x1, y0, y1, above);
        self.group_columns(&columns, y0, y1, gap, above)
    }

    fn islands_at(&self, x0: usize, x1: usize, y0: usize, y1: usize, min_luma: u32) -> Vec<Island> {
        self.islands(x0, x1, y0, y1, min_luma, ISLAND_GAP)
    }

    /// Painted elements along one horizontal band, split on [`ISLAND_GAP`].
    pub fn ink_islands(&self, x0: usize, x1: usize, y0: usize, y1: usize) -> Vec<Island> {
        self.islands_at(x0, x1, y0, y1, INK_LUMA)
    }

    /// Like [`Frame::ink_islands`], but also counts the dimmest painted pixels.
    pub fn faint_islands(&self, x0: usize, x1: usize, y0: usize, y1: usize) -> Vec<Island> {
        self.islands_at(x0, x1, y0, y1, FAINT_LUMA)
    }

    /// The near-white elements along a band — the emphasis text, such as the
    /// active lyric line, which nothing else in the list is bright enough to
    /// reach.
    pub fn bright_islands(&self, x0: usize, x1: usize, y0: usize, y1: usize) -> Vec<Island> {
        self.islands_at(x0, x1, y0, y1, BRIGHT_LUMA)
    }

    /// The violet-accent elements along a band, widest first.
    pub fn violet_islands(&self, x0: usize, x1: usize, y0: usize, y1: usize) -> Vec<Island> {
        let columns = self.matching_columns(x0, x1, y0, y1, |x, y| self.is_violet(x, y));
        let mut islands =
            self.group_columns(&columns, y0, y1, ISLAND_GAP, |x, y| self.is_violet(x, y));
        islands.sort_by_key(|island| std::cmp::Reverse(island.pixels));
        islands
    }

    /// Mean y of the painted pixels in a region, plus their count.
    pub fn centroid_y(&self, x0: usize, x1: usize, y0: usize, y1: usize) -> Option<(f64, usize)> {
        let mut sum = 0usize;
        let mut count = 0usize;
        for y in y0..y1.min(self.height) {
            for x in x0..x1.min(self.width) {
                if self.ink(x, y) {
                    sum += y;
                    count += 1;
                }
            }
        }
        (count > 0).then(|| (sum as f64 / count as f64, count))
    }

    /// Mean x of the painted pixels in a region, plus their count.
    pub fn centroid_x(&self, x0: usize, x1: usize, y0: usize, y1: usize) -> Option<(f64, usize)> {
        let mut sum = 0usize;
        let mut count = 0usize;
        for x in x0..x1.min(self.width) {
            for y in y0..y1.min(self.height) {
                if self.ink(x, y) {
                    sum += x;
                    count += 1;
                }
            }
        }
        (count > 0).then(|| (sum as f64 / count as f64, count))
    }

    /// Large uniform light-grey regions — the fingerprint of a widget still
    /// drawn with its platform theme instead of the app's.
    pub fn unthemed_blocks(&self) -> Vec<Block> {
        let mut seen = vec![false; self.width * self.height];
        let mut blocks = Vec::new();

        for start_y in 0..self.height {
            for start_x in 0..self.width {
                if seen[start_y * self.width + start_x] || !self.is_light_grey(start_x, start_y) {
                    continue;
                }

                let mut stack = vec![(start_x, start_y)];
                seen[start_y * self.width + start_x] = true;
                let mut cells: Vec<(usize, usize)> = Vec::new();

                while let Some((x, y)) = stack.pop() {
                    cells.push((x, y));
                    let neighbours = [
                        (x.checked_sub(1), Some(y)),
                        (x.checked_add(1), Some(y)),
                        (Some(x), y.checked_sub(1)),
                        (Some(x), y.checked_add(1)),
                    ];
                    for (nx, ny) in neighbours {
                        let (Some(nx), Some(ny)) = (nx, ny) else {
                            continue;
                        };
                        if nx >= self.width || ny >= self.height {
                            continue;
                        }
                        let index = ny * self.width + nx;
                        if seen[index] || !self.is_light_grey(nx, ny) {
                            continue;
                        }
                        seen[index] = true;
                        stack.push((nx, ny));
                    }
                }

                let x0 = cells.iter().map(|c| c.0).min().unwrap();
                let x1 = cells.iter().map(|c| c.0).max().unwrap();
                let y0 = cells.iter().map(|c| c.1).min().unwrap();
                let y1 = cells.iter().map(|c| c.1).max().unwrap();
                if x1 - x0 + 1 < MIN_BLOCK_W || y1 - y0 + 1 < MIN_BLOCK_H {
                    continue;
                }

                let mut histogram: HashMap<[u8; 3], usize> = HashMap::new();
                for &(x, y) in &cells {
                    *histogram.entry(self.pixel(x, y)).or_default() += 1;
                }
                let (colour, count) = histogram
                    .into_iter()
                    .max_by_key(|(_, count)| *count)
                    .expect("block has at least one pixel");
                if count as f64 / (cells.len() as f64) < MIN_UNIFORMITY {
                    continue;
                }

                blocks.push(Block {
                    x0,
                    x1,
                    y0,
                    y1,
                    pixels: cells.len(),
                    colour,
                });
            }
        }

        blocks
    }

    /// Checks that painted content in a container does not bleed outside `[container.x0..container.x1, container.y0..container.y1]`.
    /// `margin` is the region around the container to check for stray/leaked ink.
    pub fn check_text_outside_container(
        &self,
        container: Rect,
        check_margin: usize,
        label: &str,
        report: &mut Report,
    ) {
        report.increment_checks();
        let cx0 = container.x0;
        let cx1 = container.x1;
        let cy0 = container.y0;
        let cy1 = container.y1.min(self.height);

        // Check left of container
        let left_x0 = cx0.saturating_sub(check_margin);
        if left_x0 < cx0 {
            if let Some((_, x1)) = self.ink_extent_x(left_x0, cx0, cy0, cy1) {
                if cx0.saturating_sub(x1) > 1 {
                    report.record_failure(format!(
                        "{label}: text leaked outside container left (ink at x{x1} < container x{cx0})"
                    ));
                    return;
                }
            }
        }
        // Check right of container
        let right_x1 = (cx1 + check_margin).min(self.width);
        if right_x1 > cx1 {
            if let Some((x0, _)) = self.ink_extent_x(cx1, right_x1, cy0, cy1) {
                if x0.saturating_sub(cx1) > 1 {
                    report.record_failure(format!(
                        "{label}: text leaked outside container right (ink at x{x0} > container x{cx1})"
                    ));
                }
            }
        }
    }

    /// Checks that text inside a container is not abruptly clipped against `container.x1`.
    /// When text is unelided and clipped by an ancestor clip rectangle, ink pixels hit the exact
    /// boundary column with high ink density across multiple rows (the vertical sliced edge of a letter).
    pub fn check_not_clipped_at_boundary(&self, container: Rect, label: &str, report: &mut Report) {
        report.increment_checks();
        let cx1 = container.x1;
        let cy0 = container.y0;
        let cy1 = container.y1.min(self.height);

        if cx1 >= self.width || cx1 == 0 {
            return;
        }

        let boundary_x = cx1.saturating_sub(1);
        let boundary_ink_rows = (cy0..cy1).filter(|&y| self.ink(boundary_x, y)).count();

        // A sliced vertical glyph stroke typically hits 6 or more contiguous ink pixels at the clip line
        if boundary_ink_rows >= 6 {
            report.record_failure(format!(
                "{label}: text appears clipped abruptly at boundary x{boundary_x} ({boundary_ink_rows} ink pixels on boundary line without elision)"
            ));
        }
    }

    /// Detects whether any painted islands along a horizontal band intersect or have a gap smaller than `min_gap`.
    #[allow(clippy::too_many_arguments)]
    pub fn check_islands_no_overlap(
        &self,
        x0: usize,
        x1: usize,
        y0: usize,
        y1: usize,
        min_gap: usize,
        label: &str,
        report: &mut Report,
    ) {
        report.increment_checks();
        let islands = self.ink_islands(x0, x1, y0, y1);
        for pair in islands.windows(2) {
            let left = &pair[0];
            let right = &pair[1];
            if right.x0 <= left.x1 {
                report.record_failure(format!(
                    "{label}: text islands intersecting! left island [x{}..{}] intersects right island [x{}..{}] (overlap of {}px)",
                    left.x0, left.x1, right.x0, right.x1, left.x1 - right.x0 + 1
                ));
            } else {
                let gap = right.x0 - left.x1;
                if gap < min_gap {
                    report.record_failure(format!(
                        "{label}: text islands too close (collision): gap is {gap}px, expected >= {min_gap}px (left [x{}..{}], right [x{}..{}])",
                        left.x0, left.x1, right.x0, right.x1
                    ));
                }
            }
        }
    }

    /// Asserts that two adjacent bounding regions (e.g. Left column and Right column) do not intersect in ink.
    pub fn check_regions_no_overlap(
        &self,
        region_a: Rect,
        region_b: Rect,
        min_gap: usize,
        label: &str,
        report: &mut Report,
    ) {
        report.increment_checks();
        let extent_a = self.ink_extent_x(region_a.x0, region_a.x1, region_a.y0, region_a.y1);
        let extent_b = self.ink_extent_x(region_b.x0, region_b.x1, region_b.y0, region_b.y1);

        if let (Some((_, a_max)), Some((b_min, _))) = (extent_a, extent_b) {
            if a_max >= b_min {
                report.record_failure(format!(
                    "{label}: adjacent regions intersect! Region A ink ends at x{a_max}, Region B ink starts at x{b_min} (overlap of {}px)",
                    a_max - b_min + 1
                ));
            } else if b_min - a_max < min_gap {
                report.record_failure(format!(
                    "{label}: insufficient gap between regions: {gap}px between x{a_max} and x{b_min} (expected >= {min_gap}px)",
                    gap = b_min - a_max
                ));
            }
        }
    }
}

/// A specific visual layout defect recorded with its screenshot identifier.
#[derive(Clone, Debug)]
pub struct Defect {
    pub screenshot: String,
    pub message: String,
}

/// Collects check outcomes so one test run reports *every* problem it found
/// rather than stopping at the first.
#[derive(Default)]
pub struct Report {
    checks: usize,
    failures: Vec<String>,
    defects: Vec<Defect>,
}

impl Report {
    pub fn increment_checks(&mut self) {
        self.checks += 1;
    }

    pub fn record_failure(&mut self, message: impl Into<String>) {
        let msg = message.into();
        self.failures.push(msg.clone());
        let screenshot = if let Some(stripped) = msg.strip_prefix('[') {
            if let Some(end) = stripped.find(']') {
                stripped[..end].to_string()
            } else {
                "general".to_string()
            }
        } else {
            "general".to_string()
        };
        self.defects.push(Defect {
            screenshot,
            message: msg,
        });
    }

    /// Records a boolean expectation.
    pub fn require(&mut self, ok: bool, message: impl Into<String>) {
        self.checks += 1;
        if !ok {
            self.record_failure(message);
        }
    }

    pub fn defects(&self) -> &[Defect] {
        &self.defects
    }

    /// Records an expectation that a measurement lands within `tolerance`px.
    pub fn close_to(&mut self, label: &str, actual: f64, expected: f64, tolerance: f64) {
        self.require(
            (actual - expected).abs() <= tolerance,
            format!("{label}: measured {actual:.1}, expected {expected:.1} (+/-{tolerance:.1})"),
        );
    }

    /// Fails the test with every recorded failure.
    pub fn finish(self) {
        println!(
            "ui-snapshot: {} check(s) passed, {} failure(s)",
            self.checks - self.failures.len(),
            self.failures.len()
        );
        assert!(
            self.failures.is_empty(),
            "UI snapshot audit failed:\n\n{}",
            self.failures
                .iter()
                .map(|failure| format!("  - {failure}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
}

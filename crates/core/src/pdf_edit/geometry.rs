//! Rectangles in a page's own space: origin top-left, y downward, in points.

use serde::{Deserialize, Serialize};

/// An axis-aligned area of a page.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Area {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Area {
    pub fn right(&self) -> f32 {
        self.x + self.width
    }

    pub fn bottom(&self) -> f32 {
        self.y + self.height
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x <= self.right() && y >= self.y && y <= self.bottom()
    }

    /// The area grown by `dx` on its left and right and `dy` above and below.
    pub fn expanded(&self, dx: f32, dy: f32) -> Area {
        Area {
            x: self.x - dx,
            y: self.y - dy,
            width: self.width + 2.0 * dx,
            height: self.height + 2.0 * dy,
        }
    }

    pub fn translated(&self, dx: f32, dy: f32) -> Area {
        Area {
            x: self.x + dx,
            y: self.y + dy,
            ..*self
        }
    }
}

/// The parts of `bounds` that none of `keep` covers, as few rectangles as a
/// sweep down the page yields.
///
/// A page is cut into horizontal bands at every top and bottom edge in `keep`;
/// within a band the covered spans are the same from top to bottom, so what is
/// left over is a handful of rectangles. Bands that leave the same spans
/// uncovered are joined, which is what keeps a column of forty kept lines from
/// producing forty slivers.
pub fn complement(bounds: Area, keep: &[Area]) -> Vec<Area> {
    let mut edges: Vec<f32> = vec![bounds.y, bounds.bottom()];
    for area in keep {
        edges.push(area.y.clamp(bounds.y, bounds.bottom()));
        edges.push(area.bottom().clamp(bounds.y, bounds.bottom()));
    }
    edges.sort_by(f32::total_cmp);
    edges.dedup();

    let mut result: Vec<Area> = Vec::new();
    // The rectangles of the band above, still open to be extended downward.
    let mut open: Vec<usize> = Vec::new();
    for band in edges.windows(2) {
        let (top, bottom) = (band[0], band[1]);
        let mut covered: Vec<(f32, f32)> = keep
            .iter()
            .filter(|area| area.y < bottom && area.bottom() > top)
            .map(|area| {
                (
                    area.x.clamp(bounds.x, bounds.right()),
                    area.right().clamp(bounds.x, bounds.right()),
                )
            })
            .collect();
        covered.sort_by(|a, b| a.0.total_cmp(&b.0));

        let mut gaps: Vec<(f32, f32)> = Vec::new();
        let mut cursor = bounds.x;
        for (start, end) in covered {
            if start > cursor {
                gaps.push((cursor, start));
            }
            cursor = cursor.max(end);
        }
        if cursor < bounds.right() {
            gaps.push((cursor, bounds.right()));
        }

        let mut next_open = Vec::with_capacity(gaps.len());
        for (left, right) in gaps {
            let extended = open.iter().copied().find(|&index| {
                let above = &result[index];
                above.x == left && above.right() == right && above.bottom() == top
            });
            match extended {
                Some(index) => {
                    result[index].height = bottom - result[index].y;
                    next_open.push(index);
                }
                None => {
                    result.push(Area {
                        x: left,
                        y: top,
                        width: right - left,
                        height: bottom - top,
                    });
                    next_open.push(result.len() - 1);
                }
            }
        }
        open = next_open;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(x: f32, y: f32, width: f32, height: f32) -> Area {
        Area { x, y, width, height }
    }

    #[test]
    fn nothing_kept_leaves_the_whole_page() {
        let page = area(0.0, 0.0, 100.0, 200.0);
        assert_eq!(complement(page, &[]), vec![page]);
    }

    #[test]
    fn a_band_across_the_page_leaves_what_is_above_and_below() {
        let page = area(0.0, 0.0, 100.0, 200.0);
        let left = complement(page, &[area(0.0, 50.0, 100.0, 20.0)]);
        assert_eq!(left, vec![area(0.0, 0.0, 100.0, 50.0), area(0.0, 70.0, 100.0, 130.0)]);
    }

    #[test]
    fn a_kept_column_leaves_the_other_column_as_one_rectangle() {
        let page = area(0.0, 0.0, 100.0, 200.0);
        // Two stacked pieces of the left column: the right column beside them
        // is uncovered in both bands and comes back joined.
        let left = complement(
            page,
            &[area(0.0, 0.0, 40.0, 100.0), area(0.0, 100.0, 40.0, 100.0)],
        );
        assert_eq!(left, vec![area(40.0, 0.0, 60.0, 200.0)]);
    }

    #[test]
    fn overlapping_keeps_are_not_counted_twice() {
        let page = area(0.0, 0.0, 100.0, 100.0);
        let left = complement(
            page,
            &[area(10.0, 0.0, 50.0, 100.0), area(30.0, 0.0, 50.0, 100.0)],
        );
        assert_eq!(left, vec![area(0.0, 0.0, 10.0, 100.0), area(80.0, 0.0, 20.0, 100.0)]);
    }
}

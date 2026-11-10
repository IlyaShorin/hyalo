//! Where the launcher window goes on a monitor, and what is where in it.
//! Pure math, no Win32.

use crate::results::MAX_ROWS;

/// A rectangle in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// Launcher width in device-independent pixels (DIPs).
const WIDTH_DIP: f32 = 720.0;
/// Search bar height in DIPs.
pub const BAR_HEIGHT: f32 = 64.0;
/// One row of the results list, in DIPs.
pub const ROW_HEIGHT: f32 = 48.0;
/// Space above the first row and below the last, in DIPs.
const LIST_PADDING: f32 = 8.0;
/// Where the query, the titles and the subtitles start, and the room
/// right of them, in DIPs.
pub const TEXT_LEFT: f32 = 56.0;
pub const TEXT_RIGHT_PAD: f32 = 20.0;
/// How far left of where the query starts a click still places the
/// caret, in DIPs.
const TEXT_GRASP: f32 = 8.0;
/// The band round the launcher's edge that is its glass alone, in DIPs:
/// as wide as the room beside a selected row.
const EDGE: f32 = 6.0;
// The bottom edge falls in the list's padding, so no row reaches it.
const _: () = assert!(LIST_PADDING >= EDGE);
/// The launcher never takes more than this share of the monitor width.
const MAX_WIDTH_SHARE: f32 = 0.9;
/// The top edge sits this far down the work area.
const TOP_SHARE: f32 = 0.22;

/// Places the launcher on a monitor, sized for the monitor's DPI scale:
/// at `position` if the user dragged it somewhere, kept within the work
/// area; else centred horizontally, in the upper part of the work area.
/// With `rows` results it grows down from the bar, which stays where it
/// was.
pub fn launcher_rect(work_area: Rect, scale: f32, rows: usize, position: Option<Position>) -> Rect {
    let width = launcher_width(work_area, scale);
    let centre = centre_x(work_area, width);
    let (x, y) = match position {
        None => (
            centre,
            work_area.y + (work_area.height as f32 * TOP_SHARE).round() as i32,
        ),
        Some(position) => within(
            work_area,
            (width, scale),
            (
                centre + (position.x * scale).round() as i32,
                work_area.y + (position.y * scale).round() as i32,
            ),
        ),
    };
    Rect {
        x,
        y,
        width,
        height: (height(rows) * scale).round() as i32,
    }
}

/// Where the user put the launcher: its centre's offset from its work
/// area's centre, and its top's offset from the work area's top, in DIPs,
/// so that it is the same place on a monitor of another size or scale.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Position {
    pub x: f32,
    pub y: f32,
}

impl Position {
    /// The position a position file holds: two numbers.
    pub fn parse(text: &str) -> Option<Position> {
        let mut numbers = text.split_whitespace().map(|number| {
            let number = number.parse::<f32>().ok();
            number.filter(|number| number.is_finite())
        });
        let (x, y) = (numbers.next()??, numbers.next()??);
        numbers.next().is_none().then_some(Position { x, y })
    }

    /// The position as a position file holds it.
    pub fn written(self) -> String {
        format!("{} {}\n", self.x, self.y)
    }
}

/// The position of a launcher whose top left corner is at `corner`, in
/// pixels, on a monitor with `work_area` and `scale`: moved as little as
/// keeps the launcher, with its longest list, within the work area.
pub fn position_at(corner: (i32, i32), work_area: Rect, scale: f32) -> Position {
    let width = launcher_width(work_area, scale);
    let (x, y) = within(work_area, (width, scale), corner);
    Position {
        x: (x - centre_x(work_area, width)) as f32 / scale,
        y: (y - work_area.y) as f32 / scale,
    }
}

/// The launcher's width in pixels on a monitor with `work_area` and
/// `scale`: never more than most of the work area.
fn launcher_width(work_area: Rect, scale: f32) -> i32 {
    (WIDTH_DIP * scale)
        .min(work_area.width as f32 * MAX_WIDTH_SHARE)
        .round() as i32
}

/// Where a launcher `width` wide starts when it is centred.
fn centre_x(work_area: Rect, width: i32) -> i32 {
    work_area.x + (work_area.width - width) / 2
}

/// `corner` moved as little as keeps a launcher `width` pixels wide, at
/// `scale`, with its longest list, within `work_area`; at the top where
/// the work area is too short for that list.
fn within(work_area: Rect, (width, scale): (i32, f32), (x, y): (i32, i32)) -> (i32, i32) {
    let tallest = (height(MAX_ROWS) * scale).round() as i32;
    let right = (work_area.x + work_area.width - width).max(work_area.x);
    let bottom = (work_area.y + work_area.height - tallest).max(work_area.y);
    (x.clamp(work_area.x, right), y.clamp(work_area.y, bottom))
}

/// The launcher's height in DIPs: the bar, then the list if it has rows.
fn height(rows: usize) -> f32 {
    match rows {
        0 => BAR_HEIGHT,
        rows => row_top(rows) + LIST_PADDING,
    }
}

/// The top edge of row `index`, in DIPs from the top of the launcher.
pub fn row_top(index: usize) -> f32 {
    BAR_HEIGHT + LIST_PADDING + index as f32 * ROW_HEIGHT
}

/// The row at `y` DIPs from the top of the launcher, when it lists
/// `rows` rows; `None` over the bar, the padding or past the last row.
pub fn row_at(y: f32, rows: usize) -> Option<usize> {
    let below_the_bar = y - row_top(0);
    if below_the_bar < 0.0 {
        return None;
    }
    let row = (below_the_bar / ROW_HEIGHT) as usize;
    (row < rows).then_some(row)
}

/// What a point of the launcher is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    /// The query: a click places the caret.
    Text,
    /// A row of the list: a click opens it.
    Row(usize),
    /// The glass around them: the search icon, the room beside the query,
    /// the padding round the list, the edges.
    Glass,
}

/// What the point (x, y), in DIPs, of a launcher `width` DIPs wide that
/// lists `rows` rows is on. The edges are glass: the left and right ones
/// by their width, the top one above the query, and the bottom one is in
/// the list's padding, or below the query when nothing is listed.
pub fn part_at(x: f32, y: f32, width: f32, rows: usize) -> Part {
    let inside = (EDGE..width - EDGE).contains(&x);
    let text = (TEXT_LEFT - TEXT_GRASP..width - TEXT_RIGHT_PAD).contains(&x)
        && (EDGE..BAR_HEIGHT - EDGE).contains(&y);
    match row_at(y, rows) {
        _ if !inside => Part::Glass,
        _ if text => Part::Text,
        Some(row) => Part::Row(row),
        None => Part::Glass,
    }
}

/// The settings window's size, frame included, in DIPs, and the least
/// it may be made.
const SETTINGS_SIZE: (f32, f32) = (700.0, 920.0);
const SETTINGS_LEAST: (f32, f32) = (480.0, 360.0);
/// The settings window never takes more than this share of the work area.
const SETTINGS_SHARE: f32 = 0.9;

/// Places the settings window in the middle of a monitor's work area,
/// sized for the monitor's DPI scale and no larger than most of the
/// work area.
pub fn settings_rect(work_area: Rect, scale: f32) -> Rect {
    let fit =
        |dips: f32, room: i32| (dips * scale).min(room as f32 * SETTINGS_SHARE).round() as i32;
    let width = fit(SETTINGS_SIZE.0, work_area.width);
    let height = fit(SETTINGS_SIZE.1, work_area.height);
    Rect {
        x: work_area.x + (work_area.width - width) / 2,
        y: work_area.y + (work_area.height - height) / 2,
        width,
        height,
    }
}

/// The least size the settings window may be made, in pixels.
pub fn settings_least(scale: f32) -> (i32, i32) {
    let (width, height) = SETTINGS_LEAST;
    (
        (width * scale).round() as i32,
        (height * scale).round() as i32,
    )
}

/// The side of a row's icon, in DIPs.
pub const ICON_SIZE: f32 = 32.0;

/// Pixels per DIP on a monitor with `dpi` dots per inch.
pub fn scale(dpi: u32) -> f32 {
    dpi as f32 / 96.0
}

/// The side of a row's icon in pixels at `dpi`, the size to ask the shell
/// for so icons are drawn without scaling.
pub fn icon_pixels(dpi: u32) -> u32 {
    (ICON_SIZE * scale(dpi)).round() as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL_HD: Rect = Rect {
        x: 0,
        y: 0,
        width: 1920,
        height: 1040,
    };

    #[test]
    fn sits_centred_in_the_upper_part_of_the_screen() {
        let rect = launcher_rect(FULL_HD, 1.0, 0, None);
        assert_eq!(
            rect,
            Rect {
                x: 600,
                y: 229,
                width: 720,
                height: 64
            }
        );
    }

    #[test]
    fn grows_with_the_monitor_scale() {
        let work_area = Rect {
            x: 0,
            y: 0,
            width: 3840,
            height: 2100,
        };
        let rect = launcher_rect(work_area, 1.5, 0, None);
        assert_eq!(
            rect,
            Rect {
                x: 1380,
                y: 462,
                width: 1080,
                height: 96
            }
        );
    }

    #[test]
    fn leaves_a_margin_on_a_narrow_monitor() {
        let work_area = Rect {
            x: 0,
            y: 0,
            width: 600,
            height: 800,
        };
        let rect = launcher_rect(work_area, 1.0, 0, None);
        assert_eq!((rect.x, rect.width), (30, 540));
    }

    #[test]
    fn grows_down_by_a_row_per_result_plus_padding_around_the_list() {
        let bar = launcher_rect(FULL_HD, 1.0, 0, None);
        let list = launcher_rect(FULL_HD, 1.0, 3, None);
        assert_eq!((list.x, list.y, list.width), (bar.x, bar.y, bar.width));
        assert_eq!(list.height, 64 + 8 + 3 * 48 + 8);
    }

    #[test]
    fn the_list_grows_with_the_monitor_scale_too() {
        let rect = launcher_rect(FULL_HD, 1.5, 8, None);
        assert_eq!(rect.height, 696);
    }

    #[test]
    fn rows_start_below_the_bar_and_its_padding() {
        assert_eq!(row_top(0), 72.0);
        assert_eq!(row_top(2), 168.0);
    }

    #[test]
    fn a_point_on_a_row_is_on_that_row_from_its_top_edge_to_the_next() {
        assert_eq!(row_at(72.0, 3), Some(0));
        assert_eq!(row_at(119.9, 3), Some(0));
        assert_eq!(row_at(120.0, 3), Some(1));
        assert_eq!(row_at(215.9, 3), Some(2));
    }

    #[test]
    fn the_bar_the_padding_and_past_the_last_row_are_on_no_row() {
        for y in [-1.0, 0.0, 30.0, 71.9, 216.0, 300.0] {
            assert_eq!(row_at(y, 3), None, "{y}");
        }
        assert_eq!(row_at(80.0, 0), None);
    }

    #[test]
    fn the_query_takes_the_bar_from_just_before_its_text_to_its_right_room() {
        for x in [48.0, 56.0, 300.0, 699.9] {
            assert_eq!(part_at(x, 32.0, 720.0, 0), Part::Text, "{x}");
        }
        for x in [7.0, 30.0, 47.9, 700.0, 713.9] {
            assert_eq!(part_at(x, 32.0, 720.0, 0), Part::Glass, "{x}");
        }
    }

    #[test]
    fn the_edges_are_glass_all_round() {
        for (x, y) in [(5.9, 32.0), (714.0, 32.0), (300.0, 5.9), (300.0, 58.0)] {
            assert_eq!(part_at(x, y, 720.0, 0), Part::Glass, "{x}, {y}");
        }
        assert_eq!(part_at(300.0, 6.0, 720.0, 0), Part::Text);
        assert_eq!(part_at(300.0, 57.9, 720.0, 0), Part::Text);
    }

    #[test]
    fn rows_take_the_list_but_its_padding_and_edges() {
        assert_eq!(part_at(200.0, 72.0, 720.0, 3), Part::Row(0));
        assert_eq!(part_at(200.0, 150.0, 720.0, 3), Part::Row(1));
        assert_eq!(part_at(6.0, 150.0, 720.0, 3), Part::Row(1));
        assert_eq!(part_at(713.9, 215.9, 720.0, 3), Part::Row(2));
        for (x, y) in [
            (200.0, 60.0),
            (200.0, 71.9),
            (200.0, 216.0),
            (5.9, 150.0),
            (714.0, 150.0),
        ] {
            assert_eq!(part_at(x, y, 720.0, 3), Part::Glass, "{x}, {y}");
        }
        assert_eq!(
            part_at(300.0, 57.9, 720.0, 3),
            Part::Text,
            "the bar keeps its text"
        );
    }

    #[test]
    fn where_it_shows_by_default_is_a_position_too() {
        let default = launcher_rect(FULL_HD, 1.0, 0, None);
        let position = position_at((default.x, default.y), FULL_HD, 1.0);
        assert_eq!(position, Position { x: 0.0, y: 229.0 });
        assert_eq!(launcher_rect(FULL_HD, 1.0, 0, Some(position)), default);
    }

    #[test]
    fn a_position_is_its_centre_from_the_work_areas_centre_and_its_top_in_dips() {
        let work_area = Rect {
            x: -1920,
            y: 40,
            width: 1920,
            height: 1040,
        };
        let position = position_at((-1920 + 300, 40 + 150), work_area, 1.5);
        assert_eq!(position, Position { x: -80.0, y: 100.0 });
        let rect = launcher_rect(work_area, 1.5, 3, Some(position));
        assert_eq!((rect.x, rect.y), (-1920 + 300, 40 + 150));
        assert_eq!(rect.width, 1080);
        assert_eq!(rect.height, launcher_rect(work_area, 1.5, 3, None).height);
    }

    #[test]
    fn a_position_dragged_past_the_work_area_stops_where_the_tallest_list_still_fits() {
        let tallest = launcher_rect(FULL_HD, 1.0, 8, None).height;
        let corners = [(-500, -500), (5000, 5000), (5000, -500), (-500, 5000)];
        let expected = [
            (0, 0),
            (1200, 1040 - tallest),
            (1200, 0),
            (0, 1040 - tallest),
        ];
        for (corner, expected) in corners.into_iter().zip(expected) {
            let position = position_at(corner, FULL_HD, 1.0);
            let rect = launcher_rect(FULL_HD, 1.0, 0, Some(position));
            assert_eq!((rect.x, rect.y), expected, "{corner:?}");
        }
    }

    #[test]
    fn a_position_kept_from_a_larger_monitor_still_fits_a_smaller_one() {
        let small = Rect {
            x: 0,
            y: 0,
            width: 1280,
            height: 600,
        };
        let far = position_at((1200, 576 - 1), FULL_HD, 1.0);
        let rect = launcher_rect(small, 1.0, 0, Some(far));
        let tallest = launcher_rect(small, 1.0, 8, None).height;
        assert_eq!((rect.x, rect.y), (1280 - 720, 600 - tallest));
        let tiny = Rect {
            x: 10,
            y: 20,
            width: 700,
            height: 300,
        };
        let rect = launcher_rect(tiny, 1.0, 0, Some(far));
        assert_eq!(
            (rect.x, rect.y),
            (10 + 700 - 630, 20),
            "too short for the list: at the top"
        );
    }

    #[test]
    fn a_position_is_written_as_two_numbers_and_read_back() {
        let position = Position {
            x: -120.5,
            y: 300.0,
        };
        assert_eq!(position.written(), "-120.5 300\n");
        assert_eq!(Position::parse(&position.written()), Some(position));
        assert_eq!(Position::parse("  7 8 "), Some(Position { x: 7.0, y: 8.0 }));
    }

    #[test]
    fn a_position_file_that_does_not_read_is_no_position() {
        for text in ["", "7", "7 8 9", "x 8", "7 y", "NaN 8", "7 inf"] {
            assert_eq!(Position::parse(text), None, "{text:?}");
        }
    }

    #[test]
    fn the_scale_is_the_dpi_over_96() {
        assert_eq!(scale(96), 1.0);
        assert_eq!(scale(144), 1.5);
    }

    #[test]
    fn row_icons_are_32_dips_in_whole_pixels() {
        assert_eq!(icon_pixels(96), 32);
        assert_eq!(icon_pixels(120), 40);
        assert_eq!(icon_pixels(168), 56);
    }

    #[test]
    fn the_settings_window_sits_in_the_middle_of_the_work_area() {
        let rect = settings_rect(FULL_HD, 1.0);
        assert_eq!(
            rect,
            Rect {
                x: 610,
                y: 60,
                width: 700,
                height: 920
            }
        );
        let rect = settings_rect(FULL_HD, 1.5);
        assert_eq!(
            (rect.width, rect.height),
            (1050, 936),
            "no taller than 90% of it"
        );
        assert_eq!((rect.x, rect.y), (435, 52));
    }

    #[test]
    fn the_settings_window_fits_a_small_work_area_and_follows_its_monitor() {
        let small = Rect {
            x: -1280,
            y: 40,
            width: 1280,
            height: 600,
        };
        let rect = settings_rect(small, 2.0);
        assert_eq!(
            rect,
            Rect {
                x: -1280 + 64,
                y: 40 + 30,
                width: 1152,
                height: 540
            }
        );
    }

    #[test]
    fn the_settings_window_has_a_least_size_by_scale() {
        assert_eq!(settings_least(1.0), (480, 360));
        assert_eq!(settings_least(1.5), (720, 540));
    }

    #[test]
    fn follows_a_monitor_left_of_the_primary_one() {
        let left_monitor = Rect {
            x: -1920,
            y: 100,
            width: 1920,
            height: 1040,
        };
        let rect = launcher_rect(left_monitor, 1.0, 0, None);
        assert_eq!((rect.x, rect.y), (-1320, 329));
    }
}

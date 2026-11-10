//! Where the launcher window goes on a monitor. Pure math, no Win32.

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
/// The launcher never takes more than this share of the monitor width.
const MAX_WIDTH_SHARE: f32 = 0.9;
/// The top edge sits this far down the work area.
const TOP_SHARE: f32 = 0.22;

/// Places the launcher on a monitor: centred horizontally, in the upper
/// part of the work area, sized for the monitor's DPI scale. With `rows`
/// results it grows down from the bar, which stays where it was.
pub fn launcher_rect(work_area: Rect, scale: f32, rows: usize) -> Rect {
    let width = (WIDTH_DIP * scale)
        .min(work_area.width as f32 * MAX_WIDTH_SHARE)
        .round() as i32;
    Rect {
        x: work_area.x + (work_area.width - width) / 2,
        y: work_area.y + (work_area.height as f32 * TOP_SHARE).round() as i32,
        width,
        height: (height(rows) * scale).round() as i32,
    }
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
        let rect = launcher_rect(FULL_HD, 1.0, 0);
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
        let rect = launcher_rect(work_area, 1.5, 0);
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
        let rect = launcher_rect(work_area, 1.0, 0);
        assert_eq!((rect.x, rect.width), (30, 540));
    }

    #[test]
    fn grows_down_by_a_row_per_result_plus_padding_around_the_list() {
        let bar = launcher_rect(FULL_HD, 1.0, 0);
        let list = launcher_rect(FULL_HD, 1.0, 3);
        assert_eq!((list.x, list.y, list.width), (bar.x, bar.y, bar.width));
        assert_eq!(list.height, 64 + 8 + 3 * 48 + 8);
    }

    #[test]
    fn the_list_grows_with_the_monitor_scale_too() {
        let rect = launcher_rect(FULL_HD, 1.5, 8);
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
        let rect = launcher_rect(left_monitor, 1.0, 0);
        assert_eq!((rect.x, rect.y), (-1320, 329));
    }
}

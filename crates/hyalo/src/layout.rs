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

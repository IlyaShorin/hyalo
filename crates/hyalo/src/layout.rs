//! Where the launcher window goes on a monitor. Pure math, no Win32.

/// A rectangle in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// Launcher width in device-independent pixels.
const WIDTH_DIP: f32 = 720.0;
/// Search bar height in device-independent pixels.
const HEIGHT_DIP: f32 = 64.0;
/// The launcher never takes more than this share of the monitor width.
const MAX_WIDTH_SHARE: f32 = 0.9;
/// The top edge sits this far down the work area.
const TOP_SHARE: f32 = 0.22;

/// Places the launcher on a monitor: centred horizontally, in the upper
/// part of the work area, sized for the monitor's DPI scale.
pub fn launcher_rect(work_area: Rect, scale: f32) -> Rect {
    let width = (WIDTH_DIP * scale)
        .min(work_area.width as f32 * MAX_WIDTH_SHARE)
        .round() as i32;
    Rect {
        x: work_area.x + (work_area.width - width) / 2,
        y: work_area.y + (work_area.height as f32 * TOP_SHARE).round() as i32,
        width,
        height: (HEIGHT_DIP * scale).round() as i32,
    }
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
        let rect = launcher_rect(FULL_HD, 1.0);
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
        let rect = launcher_rect(work_area, 1.5);
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
        let rect = launcher_rect(work_area, 1.0);
        assert_eq!((rect.x, rect.width), (30, 540));
    }

    #[test]
    fn follows_a_monitor_left_of_the_primary_one() {
        let left_monitor = Rect {
            x: -1920,
            y: 100,
            width: 1920,
            height: 1040,
        };
        let rect = launcher_rect(left_monitor, 1.0);
        assert_eq!((rect.x, rect.y), (-1320, 329));
    }
}

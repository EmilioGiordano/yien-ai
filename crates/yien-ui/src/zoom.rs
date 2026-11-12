//! The terminals' zoom (RF-CFG-02): one offset in pixels over
//! `terminal.font.size` for every pane at once, and the size it gives.
//! Decided here without drawing; the shell applies it and the session keeps
//! the offset.

use yien_config::{MAX_FONT_SIZE, MIN_FONT_SIZE};

/// The largest offset either way that can still change the size.
const MAX_ZOOM: i32 = (MAX_FONT_SIZE - MIN_FONT_SIZE) as i32;

/// What a zoom command asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Zoom {
    /// One pixel bigger.
    In,
    /// One pixel smaller.
    Out,
    /// Back to `terminal.font.size`.
    Reset,
}

/// The terminals' font size for the configured `base` zoomed by `zoom`,
/// within the sizes the settings accept.
pub(crate) fn size(base: f32, zoom: i32) -> f32 {
    (base + zoom as f32).clamp(MIN_FONT_SIZE, MAX_FONT_SIZE)
}

/// A saved offset brought into the range where it can matter.
pub(crate) fn sanitize(zoom: i32) -> i32 {
    zoom.clamp(-MAX_ZOOM, MAX_ZOOM)
}

/// The offset after `step`. A step changes the size drawn by one pixel, or
/// leaves the offset as it is at the end of the range; an offset past the
/// end (the base changed since) is walked back first, so the first step
/// away from the end already shows.
pub(crate) fn step(base: f32, zoom: i32, step: Zoom) -> i32 {
    let by = match step {
        Zoom::In => 1,
        Zoom::Out => -1,
        Zoom::Reset => return 0,
    };
    let now = size(base, zoom);
    let mut next = sanitize(zoom);
    loop {
        next += by;
        if next.abs() > MAX_ZOOM {
            return zoom;
        }
        if size(base, next) != now {
            return next;
        }
    }
}

/// The status bar's line for a size.
pub(crate) fn notice(size: f32) -> String {
    format!("Terminal font {size} px")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rf_cfg_02_zoom_steps_one_pixel_and_resets_to_the_setting() {
        let mut zoom = 0;
        zoom = step(13.0, zoom, Zoom::In);
        zoom = step(13.0, zoom, Zoom::In);
        assert_eq!((zoom, size(13.0, zoom)), (2, 15.0));
        zoom = step(13.0, zoom, Zoom::Out);
        assert_eq!(size(13.0, zoom), 14.0);
        assert_eq!(step(13.0, zoom, Zoom::Reset), 0);
        assert_eq!(size(13.0, 0), 13.0);
        assert_eq!(step(13.0, 0, Zoom::Out), -1);
    }

    #[test]
    fn rf_cfg_02_zoom_stops_at_the_settings_range() {
        let mut zoom = 0;
        for _ in 0..100 {
            zoom = step(13.0, zoom, Zoom::In);
        }
        assert_eq!(size(13.0, zoom), MAX_FONT_SIZE);
        assert_eq!(zoom, 19, "no hidden steps past the largest size");
        for _ in 0..100 {
            zoom = step(13.0, zoom, Zoom::Out);
        }
        assert_eq!(size(13.0, zoom), MIN_FONT_SIZE);
        assert_eq!(zoom, -5);
    }

    #[test]
    fn rf_cfg_02_a_fractional_setting_still_reaches_both_ends() {
        let mut zoom = 0;
        for _ in 0..100 {
            zoom = step(13.5, zoom, Zoom::In);
        }
        assert_eq!(size(13.5, zoom), MAX_FONT_SIZE);
        zoom = step(13.5, zoom, Zoom::Out);
        assert_eq!(size(13.5, zoom), 31.5);
    }

    #[test]
    fn rf_cfg_02_a_reload_keeps_the_offset_and_the_first_step_back_shows() {
        // Zoomed in by 5 at 20 px, then the setting went to 30.
        let zoom = 5;
        assert_eq!(size(30.0, zoom), MAX_FONT_SIZE);
        let out = step(30.0, zoom, Zoom::Out);
        assert_eq!(size(30.0, out), MAX_FONT_SIZE - 1.0);
        // At the end of the range a step that cannot show leaves the offset.
        assert_eq!(step(30.0, zoom, Zoom::In), zoom);
        assert_eq!(step(8.0, -3, Zoom::Out), -3);
    }

    #[test]
    fn rf_cfg_02_a_saved_offset_out_of_range_is_brought_in() {
        assert_eq!(sanitize(i32::MAX), 24);
        assert_eq!(sanitize(i32::MIN), -24);
        assert_eq!(sanitize(-7), -7);
        assert_eq!(size(13.0, sanitize(i32::MAX)), MAX_FONT_SIZE);
        assert_eq!(step(13.0, i32::MIN, Zoom::In), -4, "the first 9 px");
    }

    #[test]
    fn rf_cfg_02_the_notice_names_the_size() {
        assert_eq!(notice(15.0), "Terminal font 15 px");
        assert_eq!(notice(13.5), "Terminal font 13.5 px");
    }
}

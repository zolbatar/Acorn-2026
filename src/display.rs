//! Hosted desktop display preferences and shared logical geometry metrics.

/// Logical size used by the hosted Wimp desktop. The host window remains a
/// separate surface and may be resized independently in a fixed mode.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(u32)]
pub enum DesktopResolution {
    /// Follow the host window's logical content size.
    #[default]
    Window = 0,
    R640x480 = 1,
    R800x600 = 2,
    R1024x768 = 3,
    R1152x864 = 4,
    R1280x1024 = 5,
    R1600x1200 = 6,
}

impl DesktopResolution {
    pub const ALL: [Self; 7] = [
        Self::Window,
        Self::R640x480,
        Self::R800x600,
        Self::R1024x768,
        Self::R1152x864,
        Self::R1280x1024,
        Self::R1600x1200,
    ];

    pub const fn id(self) -> u32 {
        self as u32
    }

    pub const fn from_id(id: u32) -> Option<Self> {
        match id {
            0 => Some(Self::Window),
            1 => Some(Self::R640x480),
            2 => Some(Self::R800x600),
            3 => Some(Self::R1024x768),
            4 => Some(Self::R1152x864),
            5 => Some(Self::R1280x1024),
            6 => Some(Self::R1600x1200),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Window => "WINDOW",
            Self::R640x480 => "640x480",
            Self::R800x600 => "800x600",
            Self::R1024x768 => "1024x768",
            Self::R1152x864 => "1152x864",
            Self::R1280x1024 => "1280x1024",
            Self::R1600x1200 => "1600x1200",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|resolution| value.eq_ignore_ascii_case(resolution.as_str()))
    }

    pub const fn fixed_size(self) -> Option<(u32, u32)> {
        match self {
            Self::Window => None,
            Self::R640x480 => Some((640, 480)),
            Self::R800x600 => Some((800, 600)),
            Self::R1024x768 => Some((1024, 768)),
            Self::R1152x864 => Some((1152, 864)),
            Self::R1280x1024 => Some((1280, 1024)),
            Self::R1600x1200 => Some((1600, 1200)),
        }
    }
}

/// Final desktop output palette. Guest BASIC modes retain their own colour
/// depth and OS_ReadPoint behavior.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(u32)]
pub enum DisplayColour {
    BW = 0,
    Grey4 = 1,
    Grey16 = 2,
    Colour16 = 3,
    Grey256 = 4,
    Colour256 = 5,
    Rgb555 = 6,
    #[default]
    Rgb888 = 7,
}

impl DisplayColour {
    pub const ALL: [Self; 8] = [
        Self::BW,
        Self::Grey4,
        Self::Grey16,
        Self::Colour16,
        Self::Grey256,
        Self::Colour256,
        Self::Rgb555,
        Self::Rgb888,
    ];

    pub const fn id(self) -> u32 {
        self as u32
    }

    pub const fn from_id(id: u32) -> Option<Self> {
        match id {
            0 => Some(Self::BW),
            1 => Some(Self::Grey4),
            2 => Some(Self::Grey16),
            3 => Some(Self::Colour16),
            4 => Some(Self::Grey256),
            5 => Some(Self::Colour256),
            6 => Some(Self::Rgb555),
            7 => Some(Self::Rgb888),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BW => "BW",
            Self::Grey4 => "4GREY",
            Self::Grey16 => "16GREY",
            Self::Colour16 => "16COLOUR",
            Self::Grey256 => "256GREY",
            Self::Colour256 => "256COLOUR",
            Self::Rgb555 => "32KRGB555",
            Self::Rgb888 => "16MRGB888",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|colour| value.eq_ignore_ascii_case(colour.as_str()))
    }
}

/// Persisted display selection, applied as one atomic preference.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DisplaySettings {
    pub resolution: DesktopResolution,
    pub colour: DisplayColour,
}

/// Shared logical desktop and host-content dimensions.
///
/// Wimp screen coordinates use two OS units per logical desktop pixel. GPU
/// framebuffer density is independent and is selected by the host renderer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DesktopMetrics {
    logical_width: u32,
    logical_height: u32,
    host_logical_width: u32,
    host_logical_height: u32,
}

impl Default for DesktopMetrics {
    fn default() -> Self {
        Self::for_display(DisplaySettings::default(), (800, 600))
    }
}

impl DesktopMetrics {
    pub fn for_display(settings: DisplaySettings, host_logical_size: (u32, u32)) -> Self {
        let host_logical_width = host_logical_size.0;
        let host_logical_height = host_logical_size.1;
        let (logical_width, logical_height) =
            settings.resolution.fixed_size().unwrap_or_else(|| {
                (
                    host_logical_width.clamp(MIN_WINDOW_WIDTH, MAX_WINDOW_DIMENSION),
                    host_logical_height.clamp(MIN_WINDOW_HEIGHT, MAX_WINDOW_DIMENSION),
                )
            });
        Self {
            logical_width,
            logical_height,
            host_logical_width,
            host_logical_height,
        }
    }

    /// Active logical desktop dimensions, before conversion to Wimp OS units.
    pub const fn pixel_size(self) -> (u32, u32) {
        (self.logical_width, self.logical_height)
    }

    /// Current host content dimensions, which can differ in a fixed mode.
    pub const fn host_pixel_size(self) -> (u32, u32) {
        (self.host_logical_width, self.host_logical_height)
    }

    pub fn os_width(self) -> i32 {
        logical_pixels_to_os_units(self.logical_width)
    }

    pub fn os_height(self) -> i32 {
        logical_pixels_to_os_units(self.logical_height)
    }
}

fn logical_pixels_to_os_units(pixels: u32) -> i32 {
    // All constructed desktop dimensions are bounded to 8192 logical pixels,
    // so conversion stays within the hosted Wimp's 16384-unit coordinate cap.
    (pixels * OS_UNITS_PER_LOGICAL_PIXEL) as i32
}

const MIN_WINDOW_WIDTH: u32 = 640;
const MIN_WINDOW_HEIGHT: u32 = 320;
const MAX_WINDOW_DIMENSION: u32 = 8192;
const OS_UNITS_PER_LOGICAL_PIXEL: u32 = 2;

#[cfg(test)]
mod tests {
    use super::{DesktopMetrics, DesktopResolution, DisplayColour, DisplaySettings};

    #[test]
    fn display_ids_are_stable_and_metrics_keep_host_and_desktop_sizes_separate() {
        assert_eq!(
            DesktopResolution::ALL.map(DesktopResolution::id),
            [0, 1, 2, 3, 4, 5, 6]
        );
        assert_eq!(
            DisplayColour::ALL.map(DisplayColour::id),
            [0, 1, 2, 3, 4, 5, 6, 7]
        );

        let windowed = DesktopMetrics::for_display(DisplaySettings::default(), (800, 600));
        assert_eq!(windowed.pixel_size(), (800, 600));
        assert_eq!(windowed.os_width(), 1600);
        assert_eq!(windowed.os_height(), 1200);

        let fixed = DesktopMetrics::for_display(
            DisplaySettings {
                resolution: DesktopResolution::R640x480,
                colour: DisplayColour::Rgb555,
            },
            (960, 700),
        );
        assert_eq!(fixed.pixel_size(), (640, 480));
        assert_eq!(fixed.host_pixel_size(), (960, 700));
        assert_eq!(fixed.os_width(), 1280);
        assert_eq!(fixed.os_height(), 960);

        let hostile = DesktopMetrics::for_display(DisplaySettings::default(), (u32::MAX, 0));
        assert_eq!(hostile.pixel_size(), (8192, 320));
        assert_eq!(hostile.host_pixel_size(), (u32::MAX, 0));
        assert_eq!(hostile.os_width(), 16_384);
        assert_eq!(hostile.os_height(), 640);
    }
}

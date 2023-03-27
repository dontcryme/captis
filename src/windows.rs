#![cfg(target_os = "windows")]

use super::{Capturer, Display};
use image::{Rgb, RgbImage};
use rayon::prelude::*;
use std::{error::Error, fmt, marker::PhantomData, mem, ptr};
use winapi::{
    shared::{
        minwindef::{BOOL, LPARAM, TRUE},
        windef::{HDC, HMONITOR, LPRECT, RECT},
    },
    um::{
        wingdi::{
            BitBlt, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDeviceCaps,
            SelectObject, BITMAPINFO, BITMAPINFOHEADER, BITSPIXEL, BI_RGB, CAPTUREBLT,
            DIB_RGB_COLORS, RGBQUAD, SRCCOPY,
        },
        winuser::{EnumDisplayMonitors, GetWindowDC},
    },
};

#[derive(Debug, Copy, Clone)]
pub enum WindowsError {
    CouldntEnumDisplayMonitors,
    CouldntGetWindowDC,
    CouldntCreateCompatibleDC,
    CouldntGetDeviceCaps,
    CouldntFindAnyDisplays,
    CouldntFindDisplay,
    CreateDIBSectionFailed,
    SelectObjectFailed,
    BitBltFailed,
    DeleteObjectFailed,
}

impl fmt::Display for WindowsError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl Error for WindowsError {}

pub(crate) struct WindowsCapturer {
    h_dc: HDC,
    h_compatible_dc: HDC,
    displays: Vec<Display>,
    primary_display_index: usize,
    bits_per_pixel: u16,
    _phantom_data: PhantomData<*const ()>,
}

impl Drop for WindowsCapturer {
    fn drop(&mut self) {
        unsafe {
            DeleteDC(self.h_compatible_dc);
            DeleteDC(self.h_dc);
        }
    }
}

impl Capturer for WindowsCapturer {
    fn displays(&self) -> &[Display] {
        &self.displays
    }

    fn refresh_displays(&mut self) -> Result<(), WindowsError> {
        let (primary_display_index, displays) = get_displays(self.h_dc)?;
        self.primary_display_index = primary_display_index;
        self.displays = displays;
        Ok(())
    }

    fn capture_primary(&self) -> Result<RgbImage, WindowsError> {
        Ok(self.capture(self.primary_display_index)?)
    }

    fn capture_all(&self) -> Result<Vec<RgbImage>, WindowsError> {
        let mut vec = Vec::with_capacity(self.displays.len());
        for i in 0..self.displays.len() {
            vec.push(self.capture(i)?);
        }
        Ok(vec)
    }

    fn capture(&self, index: usize) -> Result<RgbImage, WindowsError> {
        use WindowsError::*;

        let h_dc = self.h_dc;

        let h_compatible_dc = self.h_compatible_dc;

        let Display {
            width,
            height,
            top,
            left,
        } = *self.displays.get(index).ok_or(CouldntFindDisplay)?;

        unsafe {
            let bitmap_info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width,
                    biHeight: -height,
                    biPlanes: 1,
                    biBitCount: self.bits_per_pixel,
                    biCompression: BI_RGB,
                    ..mem::zeroed()
                },
                ..mem::zeroed()
            };

            let mut data: *mut u8 = ptr::null_mut();

            let compatible_bitmap = CreateDIBSection(
                h_dc,
                &bitmap_info as *const BITMAPINFO,
                DIB_RGB_COLORS,
                &mut data as *mut *mut u8 as _,
                ptr::null_mut(),
                0,
            );

            if compatible_bitmap.is_null() {
                return Err(CreateDIBSectionFailed);
            }

            if SelectObject(h_compatible_dc as _, compatible_bitmap as _).is_null() {
                return Err(SelectObjectFailed);
            }

            if BitBlt(
                h_compatible_dc,
                0,
                0,
                width,
                height,
                h_dc,
                left,
                top,
                SRCCOPY | CAPTUREBLT,
            ) == 0
            {
                return Err(BitBltFailed);
            }

            let slice = std::slice::from_raw_parts(data as *mut RGBQUAD, (width * height) as usize);

            let (width, height) = (width as u32, height as u32);

            let mut image: RgbImage = RgbImage::new(width, height);

            let mut i = 0;

            // for y in 0..height {
            //   for x in 0..width {
            //        let RGBQUAD {
            //            rgbBlue,
            //            rgbGreen,
            //            rgbRed,
            //            ..
            //        } = slice[i];
            //      image.put_pixel(x, y, Rgb([rgbRed, rgbGreen, rgbBlue]));
            //        i += 1;
            //    }
            //}
            (0..(width * height)).into_par_iter().for_each(|i| {
                let x = i % width;
                let y = i / width;
                let RGBQUAD { rgbBlue, rgbGreen, rgbRed, .. } = slice[i];
                image.put_pixel(x, y, Rgb([rgbRed, rgbGreen, rgbBlue]));
            });

            if DeleteObject(compatible_bitmap as _) == 0 {
                return Err(DeleteObjectFailed);
            }

            Ok(image)
        }
    }
}

impl WindowsCapturer {
    pub(crate) fn new() -> Result<Self, WindowsError> {
        use WindowsError::*;

        unsafe {
            let h_dc = GetWindowDC(ptr::null_mut());

            let (primary_display_index, displays) = get_displays(h_dc)?;

            if h_dc.is_null() {
                return Err(CouldntGetWindowDC);
            }

            let h_compatible_dc = CreateCompatibleDC(h_dc);

            if h_compatible_dc.is_null() {
                return Err(CouldntCreateCompatibleDC);
            }

            let bits_per_pixel = GetDeviceCaps(h_dc, BITSPIXEL) as u16;

            if displays.is_empty() {
                return Err(CouldntFindAnyDisplays);
            }

            Ok(Self {
                h_dc,
                h_compatible_dc,
                displays,
                primary_display_index,
                bits_per_pixel,
                _phantom_data: PhantomData,
            })
        }
    }
}

fn get_displays(h_dc: HDC) -> Result<(usize, Vec<Display>), WindowsError> {
    use WindowsError::*;

    unsafe {
        let mut displays: Vec<Display> = vec![];

        if EnumDisplayMonitors(
            h_dc,
            ptr::null_mut(),
            Some(enum_display_callback),
            (&mut displays as *mut _) as _,
        ) == 0
        {
            return Err(CouldntEnumDisplayMonitors);
        }

        if displays.is_empty() {
            return Err(CouldntFindAnyDisplays);
        }

        let primary_display_index = displays
            .iter()
            .position(|display| display.top == 0 && display.left == 0)
            .unwrap_unchecked();

        Ok((primary_display_index, displays))
    }
}

impl From<RECT> for Display {
    fn from(rect: RECT) -> Self {
        Self {
            top: rect.top,
            left: rect.left,
            width: (rect.right - rect.left).abs(),
            height: (rect.bottom - rect.top).abs(),
        }
    }
}

/// This function will give us the data we need to capture each display
/// separately through knowing each display's coordinates.
unsafe extern "system" fn enum_display_callback(
    _h_monitor: HMONITOR,
    _h_dc: HDC,
    lp_rect: LPRECT,
    l_param: LPARAM,
) -> BOOL {
    let displays = &mut *(l_param as *mut Vec<Display>);
    displays.push((*lp_rect).into());
    TRUE
}

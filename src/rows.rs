use crate::error::*;
use crate::pal::{f_pixel, gamma_lut, RGBA};
use crate::seacow::{liq_ownership, SeaCow};
use crate::LIQ_HIGH_MEMORY_LIMIT;
use std::mem::MaybeUninit;

pub(crate) type RowCallback = dyn Fn(&mut [MaybeUninit<RGBA>], usize) + Send + Sync;

pub(crate) enum PixelsSource<'pixels, 'rows> {
    Pixels { rows: SeaCow<'rows, *const RGBA>, pixels: Option<SeaCow<'pixels, RGBA>> },
    Callback(Box<RowCallback>),
}

pub(crate) struct DynamicRows<'pixels, 'rows> {
    pub(crate) width: u32,
    pub(crate) height: u32,
    f_pixels: Option<Box<[f_pixel]>>,
    pixels: PixelsSource<'pixels, 'rows>,
    pub(crate) gamma: f64,
}

pub(crate) struct DynamicRowsIter<'parent, 'pixels, 'rows> {
    px: &'parent DynamicRows<'pixels, 'rows>,
    temp_f_row: Option<Box<[MaybeUninit<f_pixel>]>>,
}

impl<'a, 'pixels, 'rows> DynamicRowsIter<'a, 'pixels, 'rows> {
    pub fn row_f<'px>(&'px mut self, temp_row: &mut [MaybeUninit<RGBA>], row: usize) -> &'px [f_pixel] {
        match self.px.f_pixels.as_ref() {
            Some(pixels) => &pixels[self.px.width as usize * row as usize..],
            None => {
                let lut = gamma_lut(self.px.gamma);
                let row_pixels = self.px.row_rgba(temp_row, row);

                let t = self.temp_f_row.as_mut().unwrap();
                DynamicRows::convert_row_to_f(t, row_pixels, &lut)
            },
        }
    }

    pub fn row_f2<'px>(&'px self, temp_row: &mut [MaybeUninit<RGBA>], temp_row_f: &'px mut [MaybeUninit<f_pixel>], row: usize) -> &'px [f_pixel] {
        match self.px.f_pixels.as_ref() {
            Some(pixels) => &pixels[self.px.width as usize * row as usize..],
            None => {
                let lut = gamma_lut(self.px.gamma);
                let row_pixels = self.px.row_rgba(temp_row, row);

                DynamicRows::convert_row_to_f(temp_row_f, row_pixels, &lut)
            },
        }
    }

    pub fn row_rgba<'px>(&'px self, temp_row: &'px mut [MaybeUninit<RGBA>], row: usize) -> &'px [RGBA] {
        self.px.row_rgba(temp_row, row)
    }
}

impl<'pixels,'rows> DynamicRows<'pixels,'rows> {
    #[inline]
    pub(crate) fn new(width: u32, height: u32, pixels: PixelsSource<'pixels, 'rows>, gamma: f64) -> Self {
        debug_assert!(gamma > 0.);
        Self { width, height, f_pixels: None, pixels, gamma }
    }

    fn row_rgba<'px>(&'px self, temp_row: &'px mut [MaybeUninit<RGBA>], row: usize) -> &[RGBA] {
        match &self.pixels {
            PixelsSource::Pixels { rows, .. } => unsafe {
                std::slice::from_raw_parts(rows.as_slice()[row], self.width())
            },
            PixelsSource::Callback(cb) => {
                cb(temp_row, row);
                // FIXME: cb needs to be marked as unsafe, since it's responsible for initialization :(
                unsafe { slice_assume_init_mut(temp_row) }
            }
        }
    }

    fn convert_row_to_f<'f>(row_f_pixels: &'f mut [MaybeUninit<f_pixel>], row_pixels: &[RGBA], gamma_lut: &[f32; 256]) -> &'f mut [f_pixel] {
        let len = row_pixels.len();
        let row_f_pixels = &mut row_f_pixels[..len];
        for (dst, src) in row_f_pixels.iter_mut().zip(row_pixels) {
            dst.write(f_pixel::from_rgba(gamma_lut, *src));
        }
        // Safe, just initialized
        unsafe { slice_assume_init_mut(row_f_pixels) }
    }

    fn should_use_low_memory(&self) -> bool {
        self.width() * self.height() > LIQ_HIGH_MEMORY_LIMIT / std::mem::size_of::<f_pixel>()
    }

    #[inline]
    fn prepare_f_pixels(&mut self, temp_row: &mut [MaybeUninit<RGBA>], allow_steamed: bool) -> Result<Option<Box<[MaybeUninit<f_pixel>]>>, liq_error> {
        debug_assert_eq!(temp_row.len(), self.width as _);
        if self.f_pixels.is_some() {
            return Ok(None);
        }

        self.prepare_generated_image(temp_row, allow_steamed)
    }

    fn prepare_generated_image(&mut self, temp_row: &mut [MaybeUninit<RGBA>], allow_steamed: bool) -> Result<Option<Box<[MaybeUninit<f_pixel>]>>, liq_error> {
        debug_assert_eq!(temp_row.len(), self.width as _);

        if allow_steamed && self.should_use_low_memory() {
            return Ok(Some(temp_buf(self.width())));
        }


        let width = self.width();
        let lut = gamma_lut(self.gamma);
        let mut f_pixels = temp_buf(self.width() * self.height());
        for (row, f_row) in f_pixels.chunks_exact_mut(width).enumerate() {
            let row_pixels = self.row_rgba(temp_row, row);
            Self::convert_row_to_f(f_row, row_pixels, &lut);
        }
        // just initialized
        self.f_pixels = Some(unsafe { box_assume_init(f_pixels) });
        Ok(None)
    }

    #[inline]
    pub fn rows_iter(&mut self, temp_row: &mut [MaybeUninit<RGBA>]) -> Result<DynamicRowsIter<'_, 'pixels, 'rows>, liq_error> {
        Ok(DynamicRowsIter {
            temp_f_row: self.prepare_f_pixels(temp_row, true)?,
            px: self,
        })
    }

    #[inline]
    pub fn rgba_rows_iter(&self) -> Result<DynamicRowsIter<'_, 'pixels, 'rows>, liq_error> {
        // This happens when histogram image is recycled
        if let PixelsSource::Pixels { rows, .. } = &self.pixels {
            if rows.as_slice().is_empty() {
                return Err(LIQ_UNSUPPORTED);
            }
        }
        Ok(DynamicRowsIter { px: self, temp_f_row: None })
    }

    #[inline]
    pub fn all_rows_f(&mut self) -> Result<&[f_pixel], liq_error> {
        if self.f_pixels.is_some() {
            return Ok(self.f_pixels.as_ref().unwrap()); // borrow-checker :(
        }
        let _ = self.prepare_f_pixels(&mut temp_buf(self.width()), false)?;
        self.f_pixels.as_deref().ok_or(LIQ_UNSUPPORTED)
    }

    /// Not recommended
    pub(crate) unsafe fn set_memory_ownership(&mut self, ownership_flags: liq_ownership) -> Result<(), liq_error> {
        let both = liq_ownership::LIQ_OWN_ROWS | liq_ownership::LIQ_OWN_PIXELS;

        if ownership_flags.is_empty() || (ownership_flags | both) != both {
            return Err(LIQ_VALUE_OUT_OF_RANGE);
        }

        if ownership_flags.contains(liq_ownership::LIQ_OWN_ROWS) {
            match &mut self.pixels {
                PixelsSource::Pixels { rows, .. } => rows.make_owned(),
                PixelsSource::Callback(_) => return Err(LIQ_VALUE_OUT_OF_RANGE),
            }
        }

        if ownership_flags.contains(liq_ownership::LIQ_OWN_PIXELS) {
            let len = self.width() * self.height();
            match &mut self.pixels {
                PixelsSource::Pixels { pixels: Some(pixels), .. } => pixels.make_owned(),
                PixelsSource::Pixels { pixels, rows } => {
                    // the row with the lowest address is assumed to be at the start of the bitmap
                    let ptr = rows.as_slice().iter().copied().min().ok_or(LIQ_UNSUPPORTED)?;
                    *pixels = Some(SeaCow::c_owned(ptr as *mut _, len));
                },
                PixelsSource::Callback(_) => return Err(LIQ_VALUE_OUT_OF_RANGE),
            }
        }
        Ok(())
    }

    pub fn free_histogram_inputs(&mut self) {
        if self.f_pixels.is_some() {
            self.pixels = PixelsSource::Pixels { rows: SeaCow::borrowed(&[]), pixels: None };
        }
    }

    #[inline(always)]
    pub fn width(&self) -> usize {
        self.width as usize
    }

    #[inline(always)]
    pub fn height(&self) -> usize {
        self.height as usize
    }
}

pub(crate) fn temp_buf<T>(len: usize) -> Box<[MaybeUninit<T>]> {
    let mut v = Vec::with_capacity(len);
    unsafe { v.set_len(len) };
    v.into_boxed_slice()
}

#[test]
fn send() {
    fn is_send<T: Send>() {}
    fn is_sync<T: Sync>() {}
    is_send::<DynamicRows>();
    is_sync::<DynamicRows>();
    is_send::<PixelsSource>();
    is_sync::<PixelsSource>();
}

#[inline(always)]
unsafe fn box_assume_init<T>(s: Box<[MaybeUninit<T>]>) -> Box<[T]> {
    std::mem::transmute(s)
}

#[inline(always)]
unsafe fn slice_assume_init_mut<T>(s: &mut [MaybeUninit<T>]) -> &mut [T] {
    std::mem::transmute(s)
}

use crate::error::{liq_error, LIQ_OK, LIQ_VALUE_OUT_OF_RANGE};
use crate::ffi::MagicTag;
use crate::ffi::LIQ_ATTR_MAGIC;
use crate::ffi::LIQ_FREED_MAGIC;
use crate::hist::Histogram;
use crate::image::Image;
use crate::pal::PalLen;
use crate::pal::RGBA;
use crate::quant::{mse_to_quality, quality_to_mse, QuantizationResult};
use crate::remap::DitherMapMode;
use std::sync::Arc;

#[derive(Clone)]
pub struct Attributes {
    pub(crate) magic_header: MagicTag,
    pub(crate) max_colors: PalLen,
    target_mse: f64,
    max_mse: Option<f64>,
    kmeans_iteration_limit: f64,
    kmeans_iterations: u16,
    feedback_loop_trials: u16,
    pub(crate) max_histogram_entries: u32,
    min_posterization_output: u8,
    min_posterization_input: u8,
    pub(crate) last_index_transparent: bool,
    pub(crate) use_contrast_maps: bool,
    pub(crate) use_dither_map: DitherMapMode,
    speed: u8,
    pub(crate) progress_stage1: u8,
    pub(crate) progress_stage2: u8,
    pub(crate) progress_stage3: u8,

    progress_callback: Option<Arc<dyn Fn(f32) -> ControlFlow + Send + Sync>>,
    log_callback: Option<Arc<dyn Fn(&Attributes, &str) + Send + Sync>>,
    log_flush_callback: Option<Arc<dyn Fn(&Attributes) + Send + Sync>>,
}

impl Attributes {
    /// New handle for library configuration
    ///
    /// See also `new_image()`
    #[inline]
    #[must_use]
    pub fn new() -> Self {
        let mut attr = Self {
            magic_header: LIQ_ATTR_MAGIC,
            target_mse: 0.,
            max_mse: None,
            max_colors: 256,
            last_index_transparent: false,
            kmeans_iteration_limit: 0.,
            max_histogram_entries: 0,
            min_posterization_output: 0,
            min_posterization_input: 0,
            kmeans_iterations: 0,
            feedback_loop_trials: 0,
            use_contrast_maps: false,
            use_dither_map: DitherMapMode::None,
            speed: 0,
            progress_stage1: 0,
            progress_stage2: 0,
            progress_stage3: 0,
            progress_callback: None,
            log_callback: None,
            log_flush_callback: None,
        };
        attr.set_speed(4);
        attr
    }

    /// It's better to use `set_quality()`
    #[inline]
    pub fn set_max_colors(&mut self, colors: u32) -> liq_error {
        if !(2..=256).contains(&colors) {
            return LIQ_VALUE_OUT_OF_RANGE;
        }
        self.max_colors = colors as PalLen;
        LIQ_OK
    }

    /// Number of least significant bits to ignore.
    ///
    /// Useful for generating palettes for VGA, 15-bit textures, or other retro platforms.
    #[inline]
    pub fn set_min_posterization(&mut self, value: u8) -> liq_error {
        if !(0..=4).contains(&value) {
            return LIQ_VALUE_OUT_OF_RANGE;
        }
        self.min_posterization_output = value;
        LIQ_OK
    }

    /// Returns number of bits of precision truncated
    #[inline(always)]
    #[must_use]
    pub fn min_posterization(&self) -> u8 {
        self.min_posterization_output
    }

    /// Range 0-100, roughly like JPEG.
    ///
    /// If minimum quality can't be met, quantization will fail.
    ///
    /// Default is min 0, max 100.
    pub fn set_quality(&mut self, minimum: u8, target: u8) -> liq_error {
        if !(0..=100).contains(&target) || target < minimum {
            return LIQ_VALUE_OUT_OF_RANGE;
        }
        self.target_mse = quality_to_mse(target);
        self.max_mse = Some(quality_to_mse(minimum));
        LIQ_OK
    }

    /// Reads values set with `set_quality`
    #[must_use]
    pub fn quality(&self) -> (u8, u8) {
        (
            self.max_mse.map(mse_to_quality).unwrap_or(0),
            mse_to_quality(self.target_mse),
        )
    }

    /// 1-10.
    ///
    /// Faster speeds generate images of lower quality, but may be useful
    /// for real-time generation of images.
    #[inline]
    pub fn set_speed(&mut self, value: i32) -> liq_error {
        if !(1..=10).contains(&value) {
            return LIQ_VALUE_OUT_OF_RANGE;
        }
        let mut iterations = (8 - value).max(0) as u16;
        iterations += iterations * iterations / 2;
        self.kmeans_iterations = iterations;
        self.kmeans_iteration_limit = 1. / ((1 << (23 - value)) as f64);
        self.feedback_loop_trials = (56 - 9 * value).max(0) as _;
        self.max_histogram_entries = ((1 << 17) + (1 << 18) * (10 - value)) as _;
        self.min_posterization_input = if value >= 8 { 1 } else { 0 };
        self.use_dither_map = if value <= 6 { DitherMapMode::Enabled } else { DitherMapMode::None };
        if self.use_dither_map != DitherMapMode::None && value < 3 {
            self.use_dither_map = DitherMapMode::Always;
        }
        self.use_contrast_maps = (value <= 7) || self.use_dither_map != DitherMapMode::None;
        self.speed = value as u8;
        self.progress_stage1 = if self.use_contrast_maps { 20 } else { 8 };
        if self.feedback_loop_trials < 2 {
            self.progress_stage1 += 30;
        }
        self.progress_stage3 = (50 / (1 + value)) as u8;
        self.progress_stage2 = 100 - self.progress_stage1 - self.progress_stage3;
        LIQ_OK
    }

    /// Move transparent color to the last entry in the palette
    ///
    /// This is less efficient for PNG, but required by some broken software
    #[inline(always)]
    pub fn set_last_index_transparent(&mut self, is_last: bool) {
        self.last_index_transparent = is_last;
    }

    /// Return currently set speed/quality trade-off setting
    #[inline(always)]
    #[must_use]
    pub fn speed(&self) -> u32 {
        self.speed.into()
    }

    /// Return max number of colors set
    #[inline(always)]
    #[must_use]
    pub fn max_colors(&self) -> u32 {
        self.max_colors.into()
    }

    /// Describe dimensions of a slice of RGBA pixels
    ///
    /// Use 0.0 for gamma if the image is sRGB (most images are).
    #[inline]
    pub fn new_image<'pixels>(&self, bitmap: &'pixels [RGBA], width: usize, height: usize, gamma: f64) -> Result<Image<'pixels, 'static>, liq_error> {
        Image::new(self, bitmap, width, height, gamma)
    }

    /// Stride is in pixels. Allows defining regions of larger images or images with padding without copying.
    #[inline]
    pub fn new_image_stride_borrow<'pixels>(&self, bitmap: &'pixels [RGBA], width: usize, height: usize, stride: usize, gamma: f64) -> Result<Image<'pixels, 'static>, liq_error> {
        Image::new_stride(self, bitmap, width, height, stride, gamma)
    }

    /// Like `new_image_stride`, but makes a copy of the pixels
    #[inline]
    pub fn new_image_stride(&self, bitmap: &[RGBA], width: usize, height: usize, stride: usize, gamma: f64) -> Result<Image<'static, 'static>, liq_error> {
        Image::new_stride_copy(self, bitmap, width, height, stride, gamma)
    }

    #[doc(hidden)]
    #[deprecated(note = "use new_image_stride")]
    #[cold]
    pub fn new_image_stride_copy(&self, bitmap: &[RGBA], width: usize, height: usize, stride: usize, gamma: f64) -> Result<Image<'static, 'static>, liq_error> {
        self.new_image_stride(bitmap, width, height, stride, gamma)
    }

    /// Generate palette for the image
    pub fn quantize(&mut self, image: &mut Image<'_, '_>) -> Result<QuantizationResult, liq_error> {
        let mut hist = Histogram::new(self);
        hist.add_image(self, image)?;
        hist.quantize_internal(self, false)
    }

    /// Set callback function to be called every time the library wants to print a message.
    ///
    /// To share data with the callback, use `Arc` or `Atomic*` types and `move ||` closures.
    #[inline]
    pub fn set_log_callback<F: Fn(&Attributes, &str) + Send + Sync + 'static>(&mut self, callback: F) {
        self.log_callback = Some(Arc::new(callback));
    }

    #[inline]
    pub fn set_log_flush_callback<F: Fn(&Attributes) + Send + Sync + 'static>(&mut self, callback: F) {
        self.log_flush_callback = Some(Arc::new(callback));
    }

    /// Set callback function to be called every time the library makes a progress.
    /// It can be used to cancel operation early.
    ///
    /// To share data with the callback, use `Arc` or `Atomic*` types and `move ||` closures.
    #[inline]
    pub fn set_progress_callback<F: Fn(f32) -> ControlFlow + Send + Sync + 'static>(&mut self, callback: F) {
        self.progress_callback = Some(Arc::new(callback));
    }

    // true == abort
    #[inline]
    pub(crate) fn progress(self: &Attributes, percent: f32) -> bool {
        if let Some(f) = &self.progress_callback {
            f(percent) == ControlFlow::Break
        } else {
            false
        }
    }

    #[inline(always)]
    pub(crate) fn verbose_print(self: &Attributes, msg: impl AsRef<str>) {
        fn _print(a: &Attributes, msg: &str) {
            if let Some(f) = &a.log_callback {
                f(a, msg)
            }
        }
        _print(self, msg.as_ref());
    }

    #[inline]
    pub(crate) fn verbose_printf_flush(self: &Attributes) {
        if let Some(f) = &self.log_flush_callback {
            f(self)
        }
    }

    pub(crate) fn feedback_loop_trials(&self, hist_items: usize) -> u16 {
        let mut feedback_loop_trials = self.feedback_loop_trials;
        if hist_items > 5000 {
            feedback_loop_trials = (feedback_loop_trials * 3 + 3) / 4;
        }
        if hist_items > 25000 {
            feedback_loop_trials = (feedback_loop_trials * 3 + 3) / 4;
        }
        if hist_items > 50000 {
            feedback_loop_trials = (feedback_loop_trials * 3 + 3) / 4;
        }
        if hist_items > 100000 {
            feedback_loop_trials = (feedback_loop_trials * 3 + 3) / 4;
        }
        feedback_loop_trials
    }

    /// max_mse, target_mse, user asked for perfect quality
    pub(crate) fn target_mse(&self, hist_items_len: usize) -> (Option<f64>, f64, bool) {
        let max_mse = self.max_mse.map(|mse| mse * if hist_items_len <= 256 { 0.33 } else { 1. });
        let aim_for_perfect_quality = self.target_mse == 0.;
        let mut target_mse = self.target_mse.max(((1 << self.min_posterization_output) as f64 / 1024.).powi(2));
        if let Some(max_mse) = max_mse {
            target_mse = target_mse.min(max_mse);
        }
        (max_mse, target_mse, aim_for_perfect_quality)
    }

    /// returns iterations, iteration_limit
    pub(crate) fn kmeans_iterations(&self, hist_items_len: usize, palette_error_is_known: bool) -> (u16, f64) {
        let mut iteration_limit = self.kmeans_iteration_limit;
        let mut iterations = self.kmeans_iterations;
        if hist_items_len > 5000 {
            iterations = (iterations * 3 + 3) / 4;
        }
        if hist_items_len > 25000 {
            iterations = (iterations * 3 + 3) / 4;
        }
        if hist_items_len > 50000 {
            iterations = (iterations * 3 + 3) / 4;
        }
        if hist_items_len > 100000 {
            iterations = (iterations * 3 + 3) / 4;
            iteration_limit *= 2.;
        }
        if iterations == 0 && !palette_error_is_known && self.max_mse.is_some() {
            iterations = 1;
        }
        (iterations, iteration_limit)
    }

    #[inline]
    pub(crate) fn posterize_bits(&self) -> u8 {
        self.min_posterization_output.max(self.min_posterization_input)
    }
}

impl Drop for Attributes {
    fn drop(&mut self) {
        self.verbose_printf_flush();
        self.magic_header = LIQ_FREED_MAGIC;
    }
}

impl Default for Attributes {
    #[inline(always)]
    fn default() -> Attributes {
        Attributes::new()
    }
}

/// Result of callback in [`Attributes::set_progress_callback`]
#[repr(C)]
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum ControlFlow {
    /// Continue processing as normal
    Continue = 1,
    /// Abort processing and fail
    Break = 0,
}

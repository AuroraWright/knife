//! Decodes H.264 with Decoder, using the hardware decoder where available.
//!
//! Decoder returns frames in decode order, so they are reordered here
//! using the presentation timestamps from the MP4 sample tables.

use std::{
    cell::RefCell,
    collections::BTreeMap,
    ffi::c_void,
    ptr::{self, NonNull},
};

use anyhow::{Context, Result, anyhow, bail, ensure};
use objc2_core_foundation::{CFDictionary, CFNumber, CFRetained, CFString};
use objc2_core_media::{
    CMBlockBuffer, CMFormatDescription, CMSampleBuffer, CMSampleTimingInfo, CMTime,
    CMVideoFormatDescriptionCreateFromH264ParameterSets, kCMBlockBufferAssureMemoryNowFlag,
};
use objc2_core_video::{
    CVImageBuffer, CVPixelBufferGetBaseAddressOfPlane, CVPixelBufferGetBytesPerRowOfPlane,
    CVPixelBufferGetHeightOfPlane, CVPixelBufferGetPixelFormatType, CVPixelBufferGetWidthOfPlane,
    CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
    kCVPixelBufferPixelFormatTypeKey, kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
};
use objc2_video_toolbox::{
    VTDecodeFrameFlags, VTDecodeInfoFlags, VTDecompressionOutputCallbackRecord,
    VTDecompressionSession,
};

use super::{FrameDecoder, Nv12Frame, Output, Packet, StreamInfo, copy_plane};

/// Frames held for reordering before the oldest is released even if an
/// earlier frame is missing (more than the H.264 maximum of 16 references).
const MAX_REORDER: usize = 17;

type Decoded = BTreeMap<i64, Result<Nv12Frame>>;

pub struct Decoder {
    session: CFRetained<VTDecompressionSession>,
    format: CFRetained<CMFormatDescription>,
    timescale: i32,
    /// Filled by the output callback; boxed so its address stays stable.
    decoded: Box<RefCell<Decoded>>,
    /// All presentation timestamps in presentation order.
    presentation_order: Vec<i64>,
    next: usize,
}

fn check(status: i32, what: &str) -> Result<()> {
    ensure!(status == 0, "{what} failed (OSStatus {status})");
    Ok(())
}

/// Copy a locked NV12 pixel buffer into tightly packed planes.
fn copy_frame(image: &CVImageBuffer) -> Result<Nv12Frame> {
    ensure!(
        CVPixelBufferGetPixelFormatType(image) == kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
        "Decoder returned an unexpected pixel format"
    );
    // SAFETY: `image` is a valid pixel buffer for the duration of the callback.
    check(
        unsafe { CVPixelBufferLockBaseAddress(image, CVPixelBufferLockFlags::ReadOnly) },
        "Locking decoded frame",
    )?;
    let width = CVPixelBufferGetWidthOfPlane(image, 0);
    let height = CVPixelBufferGetHeightOfPlane(image, 0);
    let planes = (0..2).map(|plane| {
        let base = CVPixelBufferGetBaseAddressOfPlane(image, plane);
        let stride = CVPixelBufferGetBytesPerRowOfPlane(image, plane);
        let rows = CVPixelBufferGetHeightOfPlane(image, plane);
        // The chroma plane interleaves Cb and Cr, so its rows are as many
        // bytes wide as the luma plane's.
        (!base.is_null() && stride >= width)
            // SAFETY: the plane is locked and holds `rows` rows `stride` apart.
            .then(|| unsafe { copy_plane(base.cast(), stride, width, rows) })
    });
    let planes: Option<Vec<_>> = planes.collect();
    // SAFETY: balanced with the lock above.
    unsafe { CVPixelBufferUnlockBaseAddress(image, CVPixelBufferLockFlags::ReadOnly) };
    let [y_plane, uv_plane] =
        <[Vec<u8>; 2]>::try_from(planes.context("Decoder returned an unreadable frame")?).unwrap();
    ensure!(
        uv_plane.len() == width * height / 2,
        "Decoder returned an unexpected chroma plane size"
    );
    Ok(Nv12Frame {
        y_plane,
        uv_plane,
        width: width as u32,
        height: height as u32,
    })
}

unsafe extern "C-unwind" fn output_callback(
    decoded: *mut c_void,
    _source_frame: *mut c_void,
    status: i32,
    _flags: VTDecodeInfoFlags,
    image: *mut CVImageBuffer,
    presentation_time: CMTime,
    _duration: CMTime,
) {
    // SAFETY: the session was created with a pointer to the decoder's
    // `decoded` cell, which outlives the session, and decoding is synchronous.
    let decoded = unsafe { &*decoded.cast::<RefCell<Decoded>>() };
    let frame = match (status, unsafe { image.as_ref() }) {
        (0, Some(image)) => copy_frame(image),
        (0, None) => Err(anyhow!("Decoder dropped a frame")),
        (status, _) => Err(anyhow!("Decoder decoding failed (OSStatus {status})")),
    };
    decoded.borrow_mut().insert(presentation_time.value, frame);
}

impl Decoder {
    /// Emit decoded frames that are next in presentation order.
    fn release(&mut self, output: &mut Output, flush: bool) -> Result<()> {
        loop {
            let mut decoded = self.decoded.borrow_mut();
            let Some(&first) = decoded.keys().next() else {
                return Ok(());
            };
            let expected = self.presentation_order.get(self.next).copied();
            let frame = if expected == Some(first) || flush || decoded.len() > MAX_REORDER {
                decoded.remove(&first).unwrap()
            } else {
                return Ok(());
            };
            drop(decoded);
            // Skip timestamps of frames that never arrived.
            while self
                .presentation_order
                .get(self.next)
                .is_some_and(|&time| time <= first)
            {
                self.next += 1;
            }
            output.emit(frame?);
        }
    }
}

impl FrameDecoder for Decoder {
    const NAME: &'static str = "VideoToolbox";

    fn new(info: &StreamInfo) -> Result<Self> {
        let parameter_sets: Vec<NonNull<u8>> = info
            .parameter_sets
            .iter()
            .map(|set| NonNull::new(set.as_ptr().cast_mut()).context("Empty H.264 parameter set"))
            .collect::<Result<_>>()?;
        let sizes: Vec<usize> = info.parameter_sets.iter().map(Vec::len).collect();
        ensure!(!sizes.is_empty(), "Movie has no H.264 parameter sets");
        let mut format = ptr::null();
        // SAFETY: the pointer and size arrays both describe `parameter_sets`.
        check(
            unsafe {
                CMVideoFormatDescriptionCreateFromH264ParameterSets(
                    None,
                    sizes.len(),
                    NonNull::new(parameter_sets.as_ptr().cast_mut()).unwrap(),
                    NonNull::new(sizes.as_ptr().cast_mut()).unwrap(),
                    i32::from(info.length_size),
                    NonNull::from(&mut format),
                )
            },
            "Creating the H.264 format description",
        )?;
        // SAFETY: created above with a +1 retain count.
        let format = unsafe {
            CFRetained::from_raw(NonNull::new(format.cast_mut()).context("No format description")?)
        };

        // SAFETY: the key is a valid static CFString.
        let key: &CFString = unsafe { kCVPixelBufferPixelFormatTypeKey };
        let pixel_format =
            CFNumber::new_i32(kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange as i32);
        let attributes = CFDictionary::<CFString, CFNumber>::from_slices(&[key], &[&pixel_format]);

        let decoded = Box::new(RefCell::new(Decoded::new()));
        let callback = VTDecompressionOutputCallbackRecord {
            decompressionOutputCallback: Some(output_callback),
            decompressionOutputRefCon: ptr::from_ref(&*decoded).cast_mut().cast(),
        };
        let mut session = ptr::null_mut();
        // SAFETY: all arguments are valid; the callback's context outlives the session.
        check(
            unsafe {
                VTDecompressionSession::create(
                    None,
                    &format,
                    None,
                    Some(attributes.as_opaque()),
                    &callback,
                    NonNull::from(&mut session),
                )
            },
            "Creating the Decoder decompression session",
        )?;
        // SAFETY: created above with a +1 retain count.
        let session = unsafe {
            CFRetained::from_raw(NonNull::new(session).context("No decompression session")?)
        };

        let mut presentation_order = info.presentation_times.clone();
        presentation_order.sort_unstable();
        Ok(Self {
            session,
            format,
            timescale: i32::try_from(info.timescale).context("Movie timescale is too large")?,
            decoded,
            presentation_order,
            next: 0,
        })
    }

    fn decode(&mut self, packet: &Packet, output: &mut Output) -> Result<()> {
        let length = packet.avcc.len();
        ensure!(length > 0, "Empty movie sample");
        let mut block = ptr::null_mut();
        // SAFETY: CoreMedia allocates `length` bytes for the block.
        check(
            unsafe {
                CMBlockBuffer::create_with_memory_block(
                    None,
                    ptr::null_mut(),
                    length,
                    None,
                    ptr::null(),
                    0,
                    length,
                    kCMBlockBufferAssureMemoryNowFlag,
                    NonNull::from(&mut block),
                )
            },
            "Allocating a sample buffer",
        )?;
        // SAFETY: created above with a +1 retain count.
        let block =
            unsafe { CFRetained::from_raw(NonNull::new(block).context("No block buffer")?) };
        // SAFETY: the block holds `length` bytes.
        check(
            unsafe {
                CMBlockBuffer::replace_data_bytes(
                    NonNull::new(packet.avcc.as_ptr().cast_mut())
                        .unwrap()
                        .cast(),
                    &block,
                    0,
                    length,
                )
            },
            "Filling a sample buffer",
        )?;
        // SAFETY: plain constructors of time values.
        let timing = unsafe {
            CMSampleTimingInfo {
                duration: CMTime::new(i64::from(packet.duration), self.timescale),
                presentationTimeStamp: CMTime::new(packet.presentation_time, self.timescale),
                decodeTimeStamp: CMTime::new(packet.decode_time as i64, self.timescale),
            }
        };
        let mut sample = ptr::null_mut();
        // SAFETY: one sample with one timing and one size entry.
        check(
            unsafe {
                CMSampleBuffer::create_ready(
                    None,
                    Some(&block),
                    Some(&self.format),
                    1,
                    1,
                    &timing,
                    1,
                    &length,
                    NonNull::from(&mut sample),
                )
            },
            "Creating a sample buffer",
        )?;
        // SAFETY: created above with a +1 retain count.
        let sample =
            unsafe { CFRetained::from_raw(NonNull::new(sample).context("No sample buffer")?) };
        // Synchronous decoding: the output callback runs before this returns.
        // SAFETY: the session and sample are valid.
        let status = unsafe {
            self.session.decode_frame(
                &sample,
                VTDecodeFrameFlags::empty(),
                ptr::null_mut(),
                ptr::null_mut(),
            )
        };
        if status != 0 {
            bail!("Decoder rejected a movie frame (OSStatus {status})");
        }
        self.release(output, false)
    }

    fn finish(&mut self, output: &mut Output) -> Result<()> {
        // SAFETY: the session is valid.
        unsafe {
            check(
                self.session.finish_delayed_frames(),
                "Finishing delayed frames",
            )?;
            check(
                self.session.wait_for_asynchronous_frames(),
                "Waiting for decoded frames",
            )?;
        }
        self.release(output, true)
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        // SAFETY: stops callbacks before `decoded` is freed.
        unsafe { self.session.invalidate() };
    }
}

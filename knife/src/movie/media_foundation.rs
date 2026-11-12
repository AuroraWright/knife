//! Decodes H.264 with the Media Foundation decoder that ships with Windows.
//! Windows "N" editions only have it with the Media Feature Pack installed.
//!
//! The decoder runs on the GPU's video engine (DXVA, through a Direct3D 11 device) when it can, else on the
//! CPU. Either way the frames are read back as NV12: H.264 decoding is exact, so both give the same pixels.

use std::{mem::ManuallyDrop, ptr};

use anyhow::{Context, Result, bail, ensure};
use windows::{
    Win32::{
        Foundation::HMODULE,
        Graphics::{
            Direct3D::D3D_DRIVER_TYPE_HARDWARE,
            Direct3D11::{
                D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11CreateDevice,
                ID3D11Device, ID3D11Multithread, ID3D11Texture2D,
            },
            Dxgi::IDXGIAdapter,
        },
        Media::MediaFoundation::{
            CLSID_MSH264DecoderMFT, IMF2DBuffer2, IMFDXGIBuffer, IMFDXGIDeviceManager, IMFMediaBuffer, IMFSample,
            IMFTransform, MF_SA_D3D11_AWARE, MF2DBuffer_LockFlags_Read, MFCreateDXGIDeviceManager,
            MFT_MESSAGE_SET_D3D_MANAGER,
            MF_E_NO_MORE_TYPES, MF_E_NOTACCEPTING, MF_E_TRANSFORM_NEED_MORE_INPUT,
            MF_E_TRANSFORM_STREAM_CHANGE, MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_SIZE,
            MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE, MF_MT_MINIMUM_DISPLAY_APERTURE, MF_MT_SUBTYPE,
            MF_VERSION, MFCreateMediaType, MFCreateMemoryBuffer, MFCreateSample, MFMediaType_Video,
            MFSTARTUP_NOSOCKET, MFShutdown, MFStartup, MFT_MESSAGE_COMMAND_DRAIN,
            MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, MFT_MESSAGE_NOTIFY_END_OF_STREAM,
            MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_OUTPUT_DATA_BUFFER,
            MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES, MFT_OUTPUT_STREAM_PROVIDES_SAMPLES, MFVideoArea,
            MFVideoFormat_H264, MFVideoFormat_NV12, MFVideoInterlace_Progressive,
        },
        System::Com::{
            CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
            CoUninitialize,
        },
    },
    core::Interface,
};

use super::{FrameDecoder, Nv12Frame, Output, Packet, StreamInfo, copy_plane};

/// COM and Media Foundation initialization for the decoder thread.
struct Runtime {
    com: bool,
    media_foundation: bool,
}

impl Runtime {
    fn start() -> Result<Self> {
        let mut runtime = Self {
            com: false,
            media_foundation: false,
        };
        // SAFETY: plain initialization calls, balanced in `drop`.
        unsafe {
            runtime.com = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
            MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET).context("Starting Media Foundation")?;
        }
        runtime.media_foundation = true;
        Ok(runtime)
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        // SAFETY: balances the successful calls in `start`.
        unsafe {
            if self.media_foundation {
                let _ = MFShutdown();
            }
            if self.com {
                CoUninitialize();
            }
        }
    }
}

/// Layout of the decoder's NV12 output buffers.
#[derive(Default)]
struct Layout {
    /// Visible area.
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    /// Rows in the luma plane, which may be padded below the visible area.
    coded_height: usize,
    /// Bytes per row when the buffer has no 2D interface.
    stride: usize,
}

/// The Direct3D 11 video device and DXGI device manager the hardware decoders share, made once: making them
/// for each decoder took tens of ms, which the frame waited for at every loop of a movie and every seek (`spawn`
/// waits until the decoder is ready).
struct Device {
    manager: IMFDXGIDeviceManager,
    _device: ID3D11Device,
}

// SAFETY: the device is multithread protected (`ID3D11Multithread`), and the device manager is made to be used
// from several threads: each decoder opens its own handle to the device through it.
unsafe impl Send for Device {}
unsafe impl Sync for Device {}

impl Device {
    /// The shared device, made on first use (None without a hardware video device).
    fn shared() -> Option<&'static Self> {
        static SHARED: std::sync::OnceLock<Option<Device>> = std::sync::OnceLock::new();
        SHARED
            .get_or_init(|| {
                Self::create()
                    .inspect_err(|e| tracing::info!("No Direct3D 11 video device for movies: {e:#}"))
                    .ok()
            })
            .as_ref()
    }

    fn create() -> Result<Self> {
        // SAFETY: COM calls on objects created here.
        unsafe {
            // Media Foundation stays started for the device manager, past the decoders' runtimes
            MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET).context("Starting Media Foundation")?;
            let mut device = None;
            D3D11CreateDevice(
                None::<&IDXGIAdapter>,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                None,
            )
            .context("Creating a Direct3D 11 video device")?;
            let device = device.context("Direct3D 11 returned no device")?;
            // the decoders use the device from threads of their own
            let _ = device.cast::<ID3D11Multithread>()?.SetMultithreadProtected(true);
            let mut token = 0;
            let mut manager = None;
            MFCreateDXGIDeviceManager(&mut token, &mut manager).context("Creating a DXGI device manager")?;
            let manager = manager.context("Media Foundation returned no device manager")?;
            manager.ResetDevice(&device, token)?;
            Ok(Self { manager, _device: device })
        }
    }

    /// Give `transform` the shared Direct3D 11 video device, which makes it decode with DXVA.
    fn attach(transform: &IMFTransform) -> Result<()> {
        // SAFETY: COM calls on a valid transform and the shared manager.
        unsafe {
            let aware = transform.GetAttributes()?.GetUINT32(&MF_SA_D3D11_AWARE).unwrap_or(0);
            ensure!(aware != 0, "The decoder cannot use Direct3D 11");
            let device = Self::shared().context("No Direct3D 11 video device")?;
            transform
                .ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, device.manager.as_raw() as usize)
                .context("Handing the device to the decoder")?;
        }
        Ok(())
    }
}

/// The luma plane's rows in a Direct3D surface's buffer: the texture's height, which drivers may align past the
/// coded height (None for a system-memory buffer).
///
/// # Safety
/// `buffer` must be a valid media buffer.
unsafe fn luma_rows(buffer: &IMFMediaBuffer) -> Option<usize> {
    // SAFETY: COM calls on a valid buffer; GetResource returns an owned reference, which `from_raw` takes.
    unsafe {
        let dxgi = buffer.cast::<IMFDXGIBuffer>().ok()?;
        let mut texture = ptr::null_mut();
        dxgi.GetResource(&ID3D11Texture2D::IID, &mut texture).ok()?;
        if texture.is_null() {
            return None;
        }
        let texture = ID3D11Texture2D::from_raw(texture);
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        texture.GetDesc(&mut desc);
        Some(desc.Height as usize)
    }
}

pub struct Decoder {
    transform: IMFTransform,
    layout: Layout,
    provides_samples: bool,
    output_size: u32,
    timescale: u64,
    /// decoding on the GPU (`Device`)
    hardware: bool,
    // Declared last so it is dropped after the transform.
    _runtime: Runtime,
}

impl Decoder {
    /// 100-nanosecond units, as used by Media Foundation.
    fn time(&self, value: i64) -> i64 {
        value * 10_000_000 / self.timescale as i64
    }

    /// Choose NV12 output and read its layout; repeated after format changes.
    fn select_output(&mut self) -> Result<()> {
        // SAFETY: COM calls on a valid transform.
        unsafe {
            let mut index = 0;
            let output_type = loop {
                let candidate = match self.transform.GetOutputAvailableType(0, index) {
                    Ok(candidate) => candidate,
                    Err(error) if error.code() == MF_E_NO_MORE_TYPES => {
                        bail!("The Media Foundation H.264 decoder has no NV12 output")
                    }
                    Err(error) => return Err(error).context("Listing decoder output formats"),
                };
                if candidate.GetGUID(&MF_MT_SUBTYPE)? == MFVideoFormat_NV12 {
                    break candidate;
                }
                index += 1;
            };
            self.transform
                .SetOutputType(0, &output_type, 0)
                .context("Selecting NV12 decoder output")?;
            let current = self.transform.GetOutputCurrentType(0)?;
            let size = current.GetUINT64(&MF_MT_FRAME_SIZE)?;
            let (coded_width, coded_height) =
                ((size >> 32) as usize, (size & 0xffff_ffff) as usize);
            let mut area = MFVideoArea::default();
            let aperture = current.GetBlob(
                &MF_MT_MINIMUM_DISPLAY_APERTURE,
                std::slice::from_raw_parts_mut(
                    ptr::from_mut(&mut area).cast(),
                    size_of::<MFVideoArea>(),
                ),
                None,
            );
            let (x, y, width, height) = match aperture {
                Ok(()) if area.Area.cx > 0 && area.Area.cy > 0 => (
                    area.OffsetX.value.max(0) as usize,
                    area.OffsetY.value.max(0) as usize,
                    area.Area.cx as usize,
                    area.Area.cy as usize,
                ),
                _ => (0, 0, coded_width, coded_height),
            };
            ensure!(
                x + width <= coded_width && y + height <= coded_height,
                "Decoder display area exceeds the frame"
            );
            let stride = current
                .GetUINT32(&MF_MT_DEFAULT_STRIDE)
                .map(|stride| (stride as i32).unsigned_abs() as usize)
                .unwrap_or(coded_width);
            self.layout = Layout {
                x,
                y,
                width,
                height,
                coded_height,
                stride,
            };
            let info = self.transform.GetOutputStreamInfo(0)?;
            let provides = (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0
                | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0) as u32;
            self.provides_samples = info.dwFlags & provides != 0;
            self.output_size = info.cbSize;
        }
        Ok(())
    }

    /// Copy the visible area of a decoded NV12 sample into packed planes (the buffers of `output`'s last frame).
    fn copy(&self, sample: &IMFSample, output: &mut Output) -> Result<Nv12Frame> {
        let layout = &self.layout;
        let (mut y_plane, mut uv_plane) = output.spare_planes();
        // SAFETY: COM calls on a valid sample; the buffer is locked while read.
        unsafe {
            let buffer: IMFMediaBuffer = sample.ConvertToContiguousBuffer()?;
            let rows = luma_rows(&buffer).unwrap_or(0).max(layout.coded_height);
            let two_d = buffer.cast::<IMF2DBuffer2>().ok();
            let mut base = ptr::null_mut();
            // bytes readable from `base` on
            let mut length = 0;
            let stride = match &two_d {
                Some(two_d) => {
                    let (mut pitch, mut start, mut size) = (0, ptr::null_mut(), 0u32);
                    two_d.Lock2DSize(MF2DBuffer_LockFlags_Read, &mut base, &mut pitch, &mut start, &mut size)?;
                    length = (start.add(size as usize) as usize).saturating_sub(base as usize) as u32;
                    pitch
                }
                None => {
                    buffer.Lock(&mut base, None, Some(&mut length))?;
                    layout.stride as i32
                }
            };
            let result = (|| {
                ensure!(stride > 0, "Bottom-up decoder output is not supported");
                let stride = stride as usize;
                ensure!(
                    stride >= layout.x + layout.width,
                    "Decoder rows are too short"
                );
                // the chroma plane follows the luma plane's rows, which a Direct3D surface may pad past the coded
                // height
                let chroma = base.add(stride * rows);
                let needed = stride * (rows + (layout.y + layout.height).div_ceil(2));
                ensure!(
                    length as usize >= needed,
                    "Decoded frame buffer is too small"
                );
                copy_plane(
                    &mut y_plane,
                    base.add(stride * layout.y + layout.x),
                    stride,
                    layout.width,
                    layout.height,
                );
                copy_plane(
                    &mut uv_plane,
                    chroma.add(stride * (layout.y / 2) + layout.x),
                    stride,
                    layout.width,
                    layout.height / 2,
                );
                Ok(Nv12Frame {
                    y_plane,
                    uv_plane,
                    width: layout.width as u32,
                    height: layout.height as u32,
                })
            })();
            match &two_d {
                Some(two_d) => two_d.Unlock2D()?,
                None => buffer.Unlock()?,
            }
            result
        }
    }

    /// Emit every frame the decoder has ready.
    fn drain(&mut self, output: &mut Output) -> Result<()> {
        loop {
            // SAFETY: COM calls on a valid transform; the output buffer's
            // fields are taken back after `ProcessOutput`.
            let (result, sample) = unsafe {
                let sample = if self.provides_samples {
                    None
                } else {
                    let sample = MFCreateSample()?;
                    sample.AddBuffer(&MFCreateMemoryBuffer(self.output_size)?)?;
                    Some(sample)
                };
                let mut buffers = [MFT_OUTPUT_DATA_BUFFER {
                    dwStreamID: 0,
                    pSample: ManuallyDrop::new(sample),
                    dwStatus: 0,
                    pEvents: ManuallyDrop::new(None),
                }];
                let mut status = 0;
                let result = self.transform.ProcessOutput(0, &mut buffers, &mut status);
                let [mut buffer] = buffers;
                ManuallyDrop::drop(&mut buffer.pEvents);
                (result, ManuallyDrop::take(&mut buffer.pSample))
            };
            match result {
                Ok(()) => {
                    let sample = sample.context("The decoder produced no frame")?;
                    let frame = self.copy(&sample, output)?;
                    output.emit(frame);
                }
                Err(error) if error.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(()),
                Err(error) if error.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                    self.select_output()?
                }
                Err(error) => return Err(error).context("Reading a decoded frame"),
            }
        }
    }
}

impl Decoder {
    /// A decoder on the GPU (`hardware`) or the CPU.
    fn create(info: &StreamInfo, hardware: bool) -> Result<Self> {
        let runtime = Runtime::start()?;
        // SAFETY: COM calls on objects created here.
        let transform: IMFTransform = unsafe {
            CoCreateInstance(&CLSID_MSH264DecoderMFT, None, CLSCTX_INPROC_SERVER).context(
                "Creating the Media Foundation H.264 decoder \
                 (Windows N editions need the Media Feature Pack)",
            )?
        };
        if hardware {
            Device::attach(&transform)?;
        }
        // SAFETY: as above.
        unsafe {
            let input = MFCreateMediaType()?;
            input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            input.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
            input.SetUINT64(
                &MF_MT_FRAME_SIZE,
                (u64::from(info.width) << 32) | u64::from(info.height),
            )?;
            input.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            transform
                .SetInputType(0, &input, 0)
                .context("Configuring the H.264 decoder input")?;
        }
        let mut decoder = Self {
            transform,
            layout: Layout::default(),
            provides_samples: false,
            output_size: 0,
            timescale: u64::from(info.timescale).max(1),
            hardware,
            _runtime: runtime,
        };
        decoder.select_output()?;
        // SAFETY: as above.
        unsafe {
            decoder
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            decoder
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
        }
        Ok(decoder)
    }
}

impl FrameDecoder for Decoder {
    const NAME: &'static str = "Media Foundation";

    fn new(info: &StreamInfo) -> Result<Self> {
        Self::create(info, true).or_else(|e| {
            tracing::info!("Media Foundation hardware decoding unavailable, decoding in software: {e:#}");
            Self::software(info)
        })
    }

    fn hardware(&self) -> bool {
        self.hardware
    }

    fn software(info: &StreamInfo) -> Result<Self> {
        Self::create(info, false)
    }

    fn decode(&mut self, packet: &Packet, output: &mut Output) -> Result<()> {
        let length = u32::try_from(packet.annex_b.len()).context("Movie sample is too large")?;
        // SAFETY: COM calls on objects created here; the buffer is locked while written.
        let sample = unsafe {
            let buffer = MFCreateMemoryBuffer(length)?;
            let mut data = ptr::null_mut();
            buffer.Lock(&mut data, None, None)?;
            ptr::copy_nonoverlapping(packet.annex_b.as_ptr(), data, packet.annex_b.len());
            buffer.Unlock()?;
            buffer.SetCurrentLength(length)?;
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(self.time(packet.presentation_time))?;
            sample.SetSampleDuration(self.time(i64::from(packet.duration)))?;
            sample
        };
        for _ in 0..2 {
            // SAFETY: COM call on a valid transform and sample.
            match unsafe { self.transform.ProcessInput(0, &sample, 0) } {
                Ok(()) => return self.drain(output),
                // The decoder is full: collect its frames, then retry once.
                Err(error) if error.code() == MF_E_NOTACCEPTING => self.drain(output)?,
                Err(error) => return Err(error).context("Sending a frame to the decoder"),
            }
        }
        bail!("The Media Foundation decoder stopped accepting frames")
    }

    fn finish(&mut self, output: &mut Output) -> Result<()> {
        // SAFETY: COM calls on a valid transform.
        unsafe {
            self.transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0)?;
            self.transform
                .ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0)?;
        }
        self.drain(output)
    }
}
